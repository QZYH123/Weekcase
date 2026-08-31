use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::state::format_rfc3339_utc;

pub const PROTOCOL_VERSION: u8 = 1;
pub const UNDO_CAPACITY: usize = 200;
pub const UNDO_MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JournalOp {
    Move,
    Undo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRecord {
    pub v: u8,
    pub id: String,
    pub ts: String,
    pub op: JournalOp,
    pub from: PathBuf,
    pub to: PathBuf,
    pub size: u64,
    pub source_id: String,
}

#[derive(Debug)]
pub enum UndoError {
    Empty,
    SourceExists,
    SplitCopy,
    Io(io::Error),
}

impl From<io::Error> for UndoError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub fn parse_records(text: &str) -> Vec<JournalRecord> {
    text.lines().filter_map(parse_line).collect()
}

pub fn read_records(path: &Path) -> io::Result<Vec<JournalRecord>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse_records(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Last `op=move` with no later `op=undo` sharing that id.
pub fn last_undoable(records: &[JournalRecord]) -> Option<&JournalRecord> {
    let mut undone: HashSet<&str> = HashSet::new();
    for rec in records.iter().rev() {
        match rec.op {
            JournalOp::Undo => {
                undone.insert(rec.id.as_str());
            }
            JournalOp::Move => {
                if !undone.contains(rec.id.as_str()) {
                    return Some(rec);
                }
            }
        }
    }
    None
}

pub fn append_record(path: &Path, rec: &JournalRecord) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    let mut buf = Vec::new();
    serde_json::to_writer(&mut buf, rec)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    buf.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&buf)?;
    file.sync_all()?;
    Ok(())
}

/// Keep the newest `capacity` moves and every undo whose id points at those moves.
pub fn compact_journal(path: &Path, capacity: usize, max_bytes: u64) -> io::Result<bool> {
    let len = match fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let records = read_records(path)?;
    let move_count = records.iter().filter(|r| r.op == JournalOp::Move).count();
    if len <= max_bytes && move_count <= capacity {
        return Ok(false);
    }
    let kept = compact_records(&records, capacity);
    write_records(path, &kept)?;
    Ok(true)
}

pub fn compact_records(records: &[JournalRecord], capacity: usize) -> Vec<JournalRecord> {
    let mut kept_ids: Vec<&str> = Vec::new();
    for rec in records.iter().rev() {
        if rec.op == JournalOp::Move {
            kept_ids.push(rec.id.as_str());
            if kept_ids.len() >= capacity {
                break;
            }
        }
    }
    let kept: HashSet<&str> = kept_ids.into_iter().collect();
    records
        .iter()
        .filter(|r| kept.contains(r.id.as_str()))
        .cloned()
        .collect()
}

pub(crate) fn new_move_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{nanos:016x}-{:08x}", SEQ.fetch_add(1, Ordering::Relaxed))
}

pub(crate) fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_rfc3339_utc(secs)
}

pub(crate) fn undo_record(mv: &JournalRecord) -> JournalRecord {
    JournalRecord {
        v: PROTOCOL_VERSION,
        id: mv.id.clone(),
        ts: now_rfc3339(),
        op: JournalOp::Undo,
        from: mv.from.clone(),
        to: mv.to.clone(),
        size: mv.size,
        source_id: mv.source_id.clone(),
    }
}

fn parse_line(line: &str) -> Option<JournalRecord> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let rec: JournalRecord = serde_json::from_str(line).ok()?;
    (rec.v == PROTOCOL_VERSION).then_some(rec)
}

fn write_records(path: &Path, records: &[JournalRecord]) -> io::Result<()> {
    let mut buf = Vec::new();
    for rec in records {
        serde_json::to_writer(&mut buf, rec)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        buf.push(b'\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut file = File::create(&tmp)?;
        file.write_all(&buf)?;
        file.sync_all()?;
    }
    replace_journal(&tmp, path)
}

/// Atomically put `tmp` over `dest`. Failure leaves `dest` untouched and keeps `tmp`.
/// REPLACE_EXISTING is allowed only for this journal file, never for user archives.
fn replace_journal(tmp: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };

        let from_w = to_wide(tmp);
        let to_w = to_wide(dest);
        // SAFETY: both buffers are NUL-terminated UTF-16 and live for this call.
        unsafe {
            MoveFileExW(
                PCWSTR::from_raw(from_w.as_ptr()),
                PCWSTR::from_raw(to_w.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(win_to_io)
    }
    #[cfg(not(windows))]
    {
        fs::rename(tmp, dest)
    }
}

#[cfg(windows)]
fn to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(core::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn win_to_io(e: windows::core::Error) -> io::Error {
    let hr = e.code().0 as u32;
    let code = if hr & 0xFFFF_0000 == 0x8007_0000 {
        (hr & 0xFFFF) as i32
    } else {
        e.code().0
    };
    io::Error::from_raw_os_error(code)
}

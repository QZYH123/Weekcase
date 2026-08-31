use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::classify::{FileSnapshot, Placement};
use crate::config::{Collision, Config};
use crate::state::{same_path, AppState};
use crate::undo::{
    append_record, last_undoable, new_move_id, now_rfc3339, read_records, undo_record, JournalOp,
    JournalRecord, UndoError, PROTOCOL_VERSION,
};

pub const MAX_COLLISION_SUFFIX: u32 = 99;

#[derive(Debug)]
pub enum ExecError {
    SamePath,
    Blocked,
    Skipped,
    CollisionLimit,
    SharingViolation,
    DiskFull,
    AccessDenied,
    CopyLeftSource,
    SplitCopy,
    Io(io::Error),
}

impl From<io::Error> for ExecError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub fn suffixed_file_name(name: &OsStr, n: u32) -> OsString {
    if n == 0 {
        return name.to_os_string();
    }
    let s = name.to_string_lossy();
    match s.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => {
            format!("{stem}-{n}.{ext}").into()
        }
        _ => format!("{s}-{n}").into(),
    }
}

pub fn resolve_dest(
    dest_dir: &Path,
    dest_name: &OsStr,
    collision: Collision,
    mut exists: impl FnMut(&Path) -> bool,
) -> Result<PathBuf, ExecError> {
    let original = dest_dir.join(dest_name);
    match collision {
        Collision::Skip => {
            if exists(&original) {
                Err(ExecError::Skipped)
            } else {
                Ok(original)
            }
        }
        Collision::Suffix => {
            for n in 0..=MAX_COLLISION_SUFFIX {
                let dest = dest_dir.join(suffixed_file_name(dest_name, n));
                if !exists(&dest) {
                    return Ok(dest);
                }
            }
            Err(ExecError::CollisionLimit)
        }
    }
}

pub fn execute_move(
    cfg: &Config,
    snap: &FileSnapshot,
    placement: &Placement,
    undo_path: &Path,
    state_path: &Path,
) -> Result<JournalRecord, ExecError> {
    let unsuffixed = placement.dest_dir.join(&placement.dest_name);
    if same_path(&snap.path, &unsuffixed) {
        return Err(ExecError::SamePath);
    }
    let state = AppState::load(state_path)?;
    if state.is_blocked_from(&snap.path) {
        return Err(ExecError::Blocked);
    }

    fs::create_dir_all(&placement.dest_dir)?;

    if cfg.destination.collision == Collision::Skip {
        let dest = placement.dest_dir.join(&placement.dest_name);
        if path_exists(&dest) {
            return skip_collision(state_path, snap.path.clone());
        }
        return match try_move(snap, &dest, undo_path, state_path) {
            Err(ExecError::Io(e)) if is_dest_occupied(&e) => {
                skip_collision(state_path, snap.path.clone())
            }
            other => other,
        };
    }

    for n in 0..=MAX_COLLISION_SUFFIX {
        let dest = placement
            .dest_dir
            .join(suffixed_file_name(&placement.dest_name, n));
        if path_exists(&dest) {
            continue;
        }
        match try_move(snap, &dest, undo_path, state_path) {
            Err(ExecError::Io(e)) if is_dest_occupied(&e) => continue,
            other => return other,
        }
    }
    Err(ExecError::CollisionLimit)
}

pub fn undo_last(undo_path: &Path) -> Result<JournalRecord, UndoError> {
    let records = read_records(undo_path)?;
    let Some(mv) = last_undoable(&records) else {
        return Err(UndoError::Empty);
    };
    if !path_exists(&mv.to) {
        tracing::warn!(to = %mv.to.display(), "文件已不在落点");
        let rec = undo_record(mv);
        append_record(undo_path, &rec)?;
        return Ok(rec);
    }
    if path_exists(&mv.from) {
        return Err(UndoError::SourceExists);
    }
    let move_result = move_file(&mv.to, &mv.from, false);
    settle_undo(&mv.from, &mv.to, move_result)?;
    let rec = undo_record(mv);
    append_record(undo_path, &rec)?;
    tracing::info!(from = %mv.from.display(), to = %mv.to.display(), "undone");
    Ok(rec)
}

fn try_move(
    snap: &FileSnapshot,
    dest: &Path,
    undo_path: &Path,
    state_path: &Path,
) -> Result<JournalRecord, ExecError> {
    let from = snap.path.as_path();
    let move_result = move_file(from, dest, true);
    settle_after_move(from, dest, move_result, state_path)?;
    if let Some(size) = file_size(dest) {
        if size != snap.size {
            tracing::error!(
                dest = %dest.display(),
                expected = snap.size,
                actual = size,
                "destination size mismatch"
            );
        }
    }
    let rec = JournalRecord {
        v: PROTOCOL_VERSION,
        id: new_move_id(),
        ts: now_rfc3339(),
        op: JournalOp::Move,
        from: snap.path.clone(),
        to: dest.to_path_buf(),
        size: snap.size,
        source_id: snap.source_id.clone(),
    };
    append_record(undo_path, &rec)?;
    tracing::info!(from = %from.display(), to = %dest.display(), size = snap.size, "moved");
    Ok(rec)
}

fn settle_after_move(
    from: &Path,
    to: &Path,
    move_result: io::Result<()>,
    state_path: &Path,
) -> Result<(), ExecError> {
    let from_ex = path_exists(from);
    let to_ex = path_exists(to);
    match move_result {
        Ok(()) => {
            if from_ex && to_ex {
                return handle_split(from, to, state_path);
            }
            if to_ex && !from_ex {
                return Ok(());
            }
            Err(ExecError::Io(io::Error::new(
                io::ErrorKind::Other,
                "move reported success but destination is missing",
            )))
        }
        Err(e) => {
            if is_dest_occupied(&e) {
                return Err(ExecError::Io(e));
            }
            if to_ex && !from_ex {
                return Ok(());
            }
            if to_ex && from_ex {
                return handle_split(from, to, state_path);
            }
            Err(map_move_err(e))
        }
    }
}

fn settle_undo(from: &Path, to: &Path, move_result: io::Result<()>) -> Result<(), UndoError> {
    let from_ex = path_exists(from);
    let to_ex = path_exists(to);
    match move_result {
        Ok(()) => finalize_undo(from, to, from_ex, to_ex),
        Err(e) => {
            if is_dest_occupied(&e) {
                return Err(UndoError::SourceExists);
            }
            match finalize_undo(from, to, from_ex, to_ex) {
                Ok(()) => Ok(()),
                Err(UndoError::SplitCopy) => Err(UndoError::SplitCopy),
                Err(_) => Err(UndoError::Io(e)),
            }
        }
    }
}

fn finalize_undo(from: &Path, to: &Path, from_ex: bool, to_ex: bool) -> Result<(), UndoError> {
    if from_ex && !to_ex {
        return Ok(());
    }
    if from_ex && to_ex {
        match delete_file(to) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::error!(
                    from = %from.display(),
                    to = %to.display(),
                    error = %e,
                    "undo split copy; dest not removed"
                );
                Err(UndoError::SplitCopy)
            }
        }
    } else {
        Err(UndoError::Io(io::Error::new(
            io::ErrorKind::Other,
            "undo did not restore the source",
        )))
    }
}

fn handle_split(from: &Path, to: &Path, state_path: &Path) -> Result<(), ExecError> {
    match delete_file(to) {
        Ok(()) => {
            tracing::error!(
                from = %from.display(),
                to = %to.display(),
                "cross-volume copy left source; dest removed"
            );
            Err(ExecError::CopyLeftSource)
        }
        Err(e) => {
            tracing::error!(
                from = %from.display(),
                to = %to.display(),
                error = %e,
                "split copy; blocked"
            );
            if let Err(err) = persist_blocked(state_path, from.to_path_buf(), to.to_path_buf()) {
                tracing::error!(
                    from = %from.display(),
                    to = %to.display(),
                    error = %err,
                    "persist blocked failed"
                );
            }
            Err(ExecError::SplitCopy)
        }
    }
}

fn skip_collision(state_path: &Path, path: PathBuf) -> Result<JournalRecord, ExecError> {
    persist_skipped(state_path, path.clone())?;
    tracing::info!(path = %path.display(), "skipped collision");
    Err(ExecError::Skipped)
}

fn persist_skipped(state_path: &Path, path: PathBuf) -> io::Result<()> {
    let mut state = AppState::load(state_path)?;
    state.push_skipped(path);
    state.save(state_path)
}

fn persist_blocked(state_path: &Path, from: PathBuf, to: PathBuf) -> io::Result<()> {
    let mut state = AppState::load(state_path)?;
    state.push_blocked(from, to);
    state.save(state_path)
}

fn path_exists(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};

        let wide = to_wide(path);
        // SAFETY: `wide` is NUL-terminated UTF-16 and lives for this call.
        let attr = unsafe { GetFileAttributesW(PCWSTR::from_raw(wide.as_ptr())) };
        attr != INVALID_FILE_ATTRIBUTES
    }
    #[cfg(not(windows))]
    {
        path.exists()
    }
}

fn delete_file(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            DeleteFileW, GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_READONLY,
            FILE_FLAGS_AND_ATTRIBUTES, INVALID_FILE_ATTRIBUTES,
        };

        let wide = to_wide(path);
        // SAFETY: `wide` is NUL-terminated UTF-16 and lives for this call.
        let attr = unsafe { GetFileAttributesW(PCWSTR::from_raw(wide.as_ptr())) };
        if attr != INVALID_FILE_ATTRIBUTES {
            let cleared = attr & !FILE_ATTRIBUTE_READONLY.0;
            if cleared != attr {
                // Cross-volume copy keeps READONLY; DeleteFileW would then ACCESS_DENIED.
                let _ = unsafe {
                    SetFileAttributesW(
                        PCWSTR::from_raw(wide.as_ptr()),
                        FILE_FLAGS_AND_ATTRIBUTES(cleared),
                    )
                };
            }
        }
        // SAFETY: `wide` is NUL-terminated UTF-16 and lives for this call.
        unsafe { DeleteFileW(PCWSTR::from_raw(wide.as_ptr())) }.map_err(win_to_io)
    }
    #[cfg(not(windows))]
    {
        fs::remove_file(path)
    }
}

fn move_file(from: &Path, to: &Path, write_through: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_COPY_ALLOWED, MOVEFILE_WRITE_THROUGH,
        };

        let from_w = to_wide(from);
        let to_w = to_wide(to);
        let mut flags = MOVEFILE_COPY_ALLOWED;
        if write_through {
            flags = flags | MOVEFILE_WRITE_THROUGH;
        }
        // SAFETY: both buffers are NUL-terminated UTF-16 and live for this call.
        unsafe {
            MoveFileExW(
                PCWSTR::from_raw(from_w.as_ptr()),
                PCWSTR::from_raw(to_w.as_ptr()),
                flags,
            )
        }
        .map_err(win_to_io)
    }
    #[cfg(not(windows))]
    {
        let _ = write_through;
        rename_noreplace(from, to)
    }
}

#[cfg(not(windows))]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux_renameat2_noreplace(from, to)
    }
    #[cfg(not(target_os = "linux"))]
    {
        use std::fs::{File, OpenOptions};
        use std::io::{self as io_copy, Write};
        let mut dest = OpenOptions::new().write(true).create_new(true).open(to)?;
        let mut src = File::open(from)?;
        io_copy::copy(&mut src, &mut dest)?;
        dest.flush()?;
        dest.sync_all()?;
        drop(dest);
        fs::remove_file(from)
    }
}

#[cfg(target_os = "linux")]
fn linux_renameat2_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const AT_FDCWD: i32 = -100;
    const RENAME_NOREPLACE: u32 = 1;

    extern "C" {
        fn renameat2(
            olddirfd: i32,
            oldpath: *const std::ffi::c_char,
            newdirfd: i32,
            newpath: *const std::ffi::c_char,
            flags: u32,
        ) -> i32;
    }

    fn cstr(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains interior NUL"))
    }

    let old = cstr(from)?;
    let new = cstr(to)?;
    // SAFETY: both CStrings are NUL-terminated and live for this call.
    let rc = unsafe {
        renameat2(
            AT_FDCWD,
            old.as_ptr(),
            AT_FDCWD,
            new.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn file_size(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            GetFileAttributesExW, GetFileExInfoStandard, WIN32_FILE_ATTRIBUTE_DATA,
        };

        let wide = to_wide(path);
        let mut data = WIN32_FILE_ATTRIBUTE_DATA::default();
        // SAFETY: `wide` and `data` live for this call; `data` is a WIN32_FILE_ATTRIBUTE_DATA.
        unsafe {
            GetFileAttributesExW(
                PCWSTR::from_raw(wide.as_ptr()),
                GetFileExInfoStandard,
                &mut data as *mut _ as *mut core::ffi::c_void,
            )
        }
        .ok()?;
        Some(((u64::from(data.nFileSizeHigh)) << 32) | u64::from(data.nFileSizeLow))
    }
    #[cfg(not(windows))]
    {
        path.metadata().ok().map(|m| m.len())
    }
}

fn is_dest_occupied(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::AlreadyExists {
        return true;
    }
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
        match e.raw_os_error().map(|c| c as u32) {
            Some(c) if c == ERROR_ALREADY_EXISTS.0 || c == ERROR_FILE_EXISTS.0 => return true,
            _ => {}
        }
    }
    false
}

fn map_move_err(e: io::Error) -> ExecError {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_DISK_FULL, ERROR_SHARING_VIOLATION,
        };
        match e.raw_os_error().map(|c| c as u32) {
            Some(c) if c == ERROR_SHARING_VIOLATION.0 => return ExecError::SharingViolation,
            Some(c) if c == ERROR_DISK_FULL.0 => return ExecError::DiskFull,
            Some(c) if c == ERROR_ACCESS_DENIED.0 => return ExecError::AccessDenied,
            _ => {}
        }
    }
    ExecError::Io(e)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::config::SourceKind;

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "weekcase-exec-{}-{n}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn snap_at(path: PathBuf, size: u64) -> FileSnapshot {
        FileSnapshot {
            path,
            source_id: "downloads".into(),
            source_kind: SourceKind::Downloads,
            size,
            mtime: SystemTime::now(),
            created: SystemTime::now(),
        }
    }

    fn place(dest_dir: PathBuf, name: &str) -> Placement {
        Placement {
            dest_dir,
            dest_name: OsString::from(name),
            bucket: crate::classify::Bucket::Documents,
        }
    }

    #[test]
    fn split_copy_deletes_dest_when_source_remains() {
        let dir = temp_dir();
        let from = dir.join("from.bin");
        let to = dir.join("to.bin");
        fs::write(&from, b"abc").unwrap();
        fs::write(&to, b"abc").unwrap();
        let state_path = dir.join("state.json");
        let err = handle_split(&from, &to, &state_path).unwrap_err();
        assert!(matches!(err, ExecError::CopyLeftSource));
        assert!(from.exists());
        assert!(!to.exists());
        let state = AppState::load(&state_path).unwrap();
        assert!(!state.is_blocked_from(&from));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_copy_blocks_when_dest_cannot_be_removed() {
        let dir = temp_dir();
        let from = dir.join("from.bin");
        let to = dir.join("to_dir");
        fs::write(&from, b"abc").unwrap();
        fs::create_dir(&to).unwrap();
        let state_path = dir.join("state.json");
        let err = handle_split(&from, &to, &state_path).unwrap_err();
        assert!(matches!(err, ExecError::SplitCopy));
        assert!(from.exists());
        assert!(to.exists());
        let state = AppState::load(&state_path).unwrap();
        assert!(state.is_blocked_from(&from));
        assert_eq!(state.blocked[0].to, to);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocked_from_is_not_moved() {
        let dir = temp_dir();
        let from = dir.join("a.pdf");
        let dest_dir = dir.join("out");
        fs::write(&from, b"data").unwrap();
        fs::create_dir_all(&dest_dir).unwrap();
        let state_path = dir.join("state.json");
        let mut state = AppState::default();
        state.push_blocked(from.clone(), dest_dir.join("a.pdf"));
        state.save(&state_path).unwrap();

        let cfg = Config::default();
        let snap = snap_at(from.clone(), 4);
        let err = execute_move(
            &cfg,
            &snap,
            &place(dest_dir.clone(), "a.pdf"),
            &dir.join("undo.jsonl"),
            &state_path,
        )
        .unwrap_err();
        assert!(matches!(err, ExecError::Blocked));
        assert!(from.exists());
        assert!(!dest_dir.join("a.pdf").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_then_undo_round_trip() {
        let dir = temp_dir();
        let from = dir.join("src").join("a.pdf");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, b"hello").unwrap();
        let dest_dir = dir.join("out");
        let undo = dir.join("undo.jsonl");
        let rec = execute_move(
            &Config::default(),
            &snap_at(from.clone(), 5),
            &place(dest_dir.clone(), "a.pdf"),
            &undo,
            &dir.join("state.json"),
        )
        .unwrap();
        let dest = dest_dir.join("a.pdf");
        assert!(!from.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
        assert_eq!(rec.op, JournalOp::Move);

        let undone = undo_last(&undo).unwrap();
        assert_eq!(undone.op, JournalOp::Undo);
        assert_eq!(undone.id, rec.id);
        assert!(from.exists());
        assert!(!dest.exists());
        assert!(matches!(undo_last(&undo).unwrap_err(), UndoError::Empty));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn suffix_when_dest_name_is_taken() {
        let dir = temp_dir();
        let from = dir.join("src").join("foo.pdf");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, b"new").unwrap();
        let dest_dir = dir.join("out");
        fs::create_dir_all(&dest_dir).unwrap();
        fs::write(dest_dir.join("foo.pdf"), b"old").unwrap();
        let rec = execute_move(
            &Config::default(),
            &snap_at(from.clone(), 3),
            &place(dest_dir.clone(), "foo.pdf"),
            &dir.join("undo.jsonl"),
            &dir.join("state.json"),
        )
        .unwrap();
        assert_eq!(fs::read(dest_dir.join("foo.pdf")).unwrap(), b"old");
        assert_eq!(fs::read(dest_dir.join("foo-1.pdf")).unwrap(), b"new");
        assert_eq!(rec.to.file_name().unwrap(), "foo-1.pdf");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_does_not_overwrite_existing_source() {
        let dir = temp_dir();
        let from = dir.join("src").join("a.pdf");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, b"moved").unwrap();
        let dest_dir = dir.join("dest");
        let undo = dir.join("undo.jsonl");
        execute_move(
            &Config::default(),
            &snap_at(from.clone(), 5),
            &place(dest_dir.clone(), "a.pdf"),
            &undo,
            &dir.join("state.json"),
        )
        .unwrap();
        fs::write(&from, b"new at source").unwrap();
        let dest = dest_dir.join("a.pdf");
        let err = undo_last(&undo).unwrap_err();
        assert!(matches!(err, UndoError::SourceExists));
        assert_eq!(fs::read(&dest).unwrap(), b"moved");
        assert_eq!(fs::read(&from).unwrap(), b"new at source");
        let records = read_records(&undo).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].op, JournalOp::Move);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_split_deletes_archive_copy() {
        let dir = temp_dir();
        let from = dir.join("from.bin");
        let to = dir.join("to.bin");
        fs::write(&from, b"copy").unwrap();
        fs::write(&to, b"archive").unwrap();
        settle_undo(&from, &to, Ok(())).unwrap();
        assert_eq!(fs::read(&from).unwrap(), b"copy");
        assert!(!to.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_split_leaves_both_without_success() {
        let dir = temp_dir();
        let from = dir.join("from.bin");
        let to = dir.join("to_dir");
        fs::write(&from, b"copy").unwrap();
        fs::create_dir(&to).unwrap();
        let err = settle_undo(&from, &to, Ok(())).unwrap_err();
        assert!(matches!(err, UndoError::SplitCopy));
        assert!(from.exists());
        assert!(to.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocked_from_matches_path_key() {
        let dir = temp_dir();
        let from = dir.join("a.pdf");
        fs::write(&from, b"data").unwrap();
        let dest_dir = dir.join("out");
        fs::create_dir_all(&dest_dir).unwrap();
        let mut state = AppState::default();
        let alt = PathBuf::from(from.to_string_lossy().replace('/', "\\"));
        state.push_blocked(alt, dest_dir.join("a.pdf"));
        let state_path = dir.join("state.json");
        state.save(&state_path).unwrap();
        let err = execute_move(
            &Config::default(),
            &snap_at(from.clone(), 4),
            &place(dest_dir.clone(), "a.pdf"),
            &dir.join("undo.jsonl"),
            &state_path,
        )
        .unwrap_err();
        assert!(matches!(err, ExecError::Blocked));
        assert!(from.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_missing_dest_still_appends() {
        let dir = temp_dir();
        let from = dir.join("src").join("a.pdf");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, b"hello").unwrap();
        let dest_dir = dir.join("out");
        let undo = dir.join("undo.jsonl");
        execute_move(
            &Config::default(),
            &snap_at(from.clone(), 5),
            &place(dest_dir.clone(), "a.pdf"),
            &undo,
            &dir.join("state.json"),
        )
        .unwrap();
        fs::remove_file(dest_dir.join("a.pdf")).unwrap();
        let rec = undo_last(&undo).unwrap();
        assert_eq!(rec.op, JournalOp::Undo);
        assert!(!from.exists());
        assert!(matches!(undo_last(&undo).unwrap_err(), UndoError::Empty));
        let _ = fs::remove_dir_all(&dir);
    }
}

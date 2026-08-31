use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use crate::candidate::{Candidate, FileSnapshot};
use crate::config::Config;
use crate::watch::{inspect_file, is_ignored, FileInfo, IgnoreSet};

const ZERO_BYTE_MIN_AGE: Duration = Duration::from_secs(60);

pub fn start_stabilize(
    cfg: Arc<Config>,
    candidates: Arc<Mutex<HashMap<PathBuf, Candidate>>>,
    paused: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) -> JoinHandle<()> {
    let ignore = IgnoreSet::from_process();
    thread::Builder::new()
        .name("weekcase-stab".into())
        .spawn(move || {
            let tick = Duration::from_millis(cfg.watch.tick_ms.max(1));
            let mut logged = HashSet::new();
            while !shutdown.load(Ordering::Relaxed) {
                let paused = paused.load(Ordering::Relaxed);
                let ready = tick_once(&candidates, &ignore, SystemTime::now(), paused);
                for snap in take_new_ready(&ready, &mut logged) {
                    tracing::info!(
                        path = %snap.path.display(),
                        source = %snap.source_id,
                        size = snap.size,
                        "ready_for_execute"
                    );
                }
                thread::sleep(tick);
            }
        })
        .expect("stabilize thread")
}

pub fn tick_once(
    candidates: &Mutex<HashMap<PathBuf, Candidate>>,
    ignore: &IgnoreSet,
    now: SystemTime,
    paused: bool,
) -> Vec<FileSnapshot> {
    let mut table = candidates.lock().unwrap_or_else(|e| e.into_inner());
    let paths: Vec<PathBuf> = table.keys().cloned().collect();
    let mut drop_list = Vec::new();
    let mut ready = Vec::new();

    for path in paths {
        let Some(cand) = table.get(&path) else {
            continue;
        };
        if cand.poisoned {
            continue;
        }
        match inspect_file(&path) {
            Ok(info) if should_keep(&path, &info, ignore) => {
                sample(&mut table, &path, &info, now, paused, &mut ready);
            }
            _ => drop_list.push(path),
        }
    }

    for path in drop_list {
        table.remove(&path);
    }
    ready
}

fn should_keep(path: &Path, info: &FileInfo, ignore: &IgnoreSet) -> bool {
    info.is_file && !info.placeholder && !is_ignored(path, ignore)
}

fn sample(
    table: &mut HashMap<PathBuf, Candidate>,
    path: &Path,
    info: &FileInfo,
    now: SystemTime,
    paused: bool,
    ready: &mut Vec<FileSnapshot>,
) {
    let Some(cand) = table.get_mut(path) else {
        return;
    };
    if info.size != cand.last_size || info.mtime != cand.last_mtime {
        cand.last_size = info.size;
        cand.last_mtime = info.mtime;
        cand.created = info.created;
        cand.stable_since = None;
        return;
    }
    if info.size == 0
        && now
            .duration_since(info.created)
            .map(|d| d < ZERO_BYTE_MIN_AGE)
            .unwrap_or(false)
    {
        return;
    }
    if !lock_probe_ok(path) {
        return;
    }
    cand.attempts = 0;
    if cand.stable_since.is_none() {
        cand.stable_since = Some(now);
    }
    if paused || !cand.is_ready(now) {
        return;
    }
    ready.push(cand.snapshot(path.to_path_buf()));
}

fn take_new_ready<'a>(
    ready: &'a [FileSnapshot],
    logged: &mut HashSet<PathBuf>,
) -> Vec<&'a FileSnapshot> {
    let live: HashSet<&Path> = ready.iter().map(|s| s.path.as_path()).collect();
    logged.retain(|p| live.contains(p.as_path()));
    ready
        .iter()
        .filter(|snap| logged.insert(snap.path.clone()))
        .collect()
}

fn lock_probe_ok(path: &Path) -> bool {
    #[cfg(windows)]
    {
        lock_probe_windows(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        true
    }
}

#[cfg(windows)]
fn lock_probe_windows(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, OPEN_EXISTING,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(core::iter::once(0))
        .collect();
    // FILE_SHARE_READ only: a writer still holding the file must fail this probe.
    // OPEN_REPARSE_POINT: do not follow; placeholders were already filtered by attributes.
    // SAFETY: `wide` is NUL-terminated UTF-16 and lives for the call.
    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    };
    match handle {
        Ok(h) => {
            unsafe {
                let _ = CloseHandle(h);
            }
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SourceKind;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::UNIX_EPOCH;

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("weekcase-stab-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ignore() -> IgnoreSet {
        IgnoreSet::empty()
    }

    fn insert_file(dir: &Path, name: &str, body: &[u8]) -> (PathBuf, FileInfo) {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        let info = inspect_file(&path).unwrap();
        (path, info)
    }

    fn candidate(info: &FileInfo, settle_secs: u64) -> Candidate {
        Candidate {
            source_id: "downloads".into(),
            source_kind: SourceKind::Downloads,
            first_seen: SystemTime::now(),
            last_size: info.size,
            last_mtime: info.mtime,
            created: info.created,
            stable_since: None,
            attempts: 0,
            poisoned: false,
            settle_secs,
        }
    }

    #[test]
    fn gone_and_ignored_are_dropped() {
        let dir = temp_dir();
        let (path, info) = insert_file(&dir, "gone.txt", b"x");
        let missing = dir.join("missing.txt");
        let cr = dir.join("a.crdownload");
        fs::write(&cr, b"tmp").unwrap();
        let cr_info = inspect_file(&cr).unwrap();
        fs::remove_file(&path).unwrap();

        let table = Mutex::new(HashMap::from([
            (missing.clone(), candidate(&info, 0)),
            (cr.clone(), candidate(&cr_info, 0)),
        ]));
        let ready = tick_once(&table, &ignore(), SystemTime::now(), false);
        assert!(ready.is_empty());
        assert!(table.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn size_change_resets_settle_then_ready_once() {
        let dir = temp_dir();
        let (path, info) = insert_file(&dir, "a.pdf", b"hello");
        let mut cand = candidate(&info, 0);
        cand.stable_since = Some(UNIX_EPOCH);
        cand.last_size = info.size + 10;
        let table = Mutex::new(HashMap::from([(path.clone(), cand)]));
        let now = SystemTime::now();

        let ready = tick_once(&table, &ignore(), now, false);
        assert!(ready.is_empty());
        {
            let t = table.lock().unwrap();
            let c = &t[&path];
            assert!(c.stable_since.is_none());
            assert_eq!(c.last_size, info.size);
        }

        let ready = tick_once(&table, &ignore(), now, false);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].path, path);
        let ready = tick_once(&table, &ignore(), now, false);
        assert_eq!(ready.len(), 1, "settled files stay in ready every tick");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ready_for_execute_logs_once_per_stable_period() {
        let snap = FileSnapshot {
            path: PathBuf::from("/a.pdf"),
            source_id: "downloads".into(),
            source_kind: SourceKind::Downloads,
            size: 1,
            mtime: UNIX_EPOCH,
            created: UNIX_EPOCH,
        };
        let ready = [snap.clone()];
        let mut logged = HashSet::new();
        assert_eq!(take_new_ready(&ready, &mut logged).len(), 1);
        assert_eq!(take_new_ready(&ready, &mut logged).len(), 0);
        assert!(take_new_ready(&[], &mut logged).is_empty());
        assert_eq!(take_new_ready(&ready, &mut logged).len(), 1);
    }

    #[test]
    fn zero_byte_young_file_is_not_ready() {
        let dir = temp_dir();
        let (path, info) = insert_file(&dir, "empty.dat", b"");
        assert_eq!(info.size, 0);
        let table = Mutex::new(HashMap::from([(path.clone(), candidate(&info, 0))]));
        let ready = tick_once(&table, &ignore(), SystemTime::now(), false);
        assert!(ready.is_empty());
        assert!(table.lock().unwrap()[&path].stable_since.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn paused_updates_stable_but_does_not_log_ready() {
        let dir = temp_dir();
        let (path, info) = insert_file(&dir, "a.pdf", b"x");
        let table = Mutex::new(HashMap::from([(path.clone(), candidate(&info, 0))]));
        let now = SystemTime::now();
        let ready = tick_once(&table, &ignore(), now, true);
        assert!(ready.is_empty());
        assert!(table.lock().unwrap()[&path].stable_since.is_some());
        let ready = tick_once(&table, &ignore(), now, false);
        assert_eq!(ready.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}

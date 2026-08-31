use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::candidate::{Candidate, FileSnapshot};
use crate::classify::{classify, ClassifyError};
use crate::config::Config;
use crate::execute::{execute_move, undo_last, ExecCmd, ExecError};
use crate::known_folders::KnownFolders;
use crate::state::AppState;
use crate::watch::{inspect_file, is_ignored, FileInfo, IgnoreSet};

const ZERO_BYTE_MIN_AGE: Duration = Duration::from_secs(60);
const ERROR_EVERY: Duration = Duration::from_secs(60);
const MAX_ATTEMPTS: u32 = 5;
const RETRY_BACKOFF_SECS: &[u64] = &[5, 15, 60];

#[allow(clippy::too_many_arguments)]
pub fn start_stabilize(
    cfg: Arc<Mutex<Config>>,
    candidates: Arc<Mutex<HashMap<PathBuf, Candidate>>>,
    paused: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    folders: Arc<Mutex<KnownFolders>>,
    undo_path: PathBuf,
    state_path: PathBuf,
    state: Arc<Mutex<AppState>>,
    archived_today: Arc<AtomicU32>,
    exec: Receiver<ExecCmd>,
) -> JoinHandle<()> {
    let ignore = IgnoreSet::from_process();
    thread::Builder::new()
        .name("weekcase-stab".into())
        .spawn(move || {
            let tick = Duration::from_millis(cfg_snapshot(&cfg).watch.tick_ms.max(1));
            let mut logged = HashSet::new();
            let mut limiter = LogLimiter::default();
            let mut illegal_warned = HashSet::new();
            let mut persist_retry = false;
            while !shutdown.load(Ordering::Relaxed) {
                drain_exec(&exec, &undo_path);
                flush_persist(&state, &state_path, &mut persist_retry);
                let paused_now = paused.load(Ordering::Relaxed);
                let ready = tick_once_with(
                    &candidates,
                    &ignore,
                    SystemTime::now(),
                    paused_now,
                    Some(&state),
                    Some(&mut persist_retry),
                );
                flush_persist(&state, &state_path, &mut persist_retry);
                for snap in take_new_ready(&ready, &mut logged) {
                    tracing::info!(
                        path = %snap.path.display(),
                        source = %snap.source_id,
                        size = snap.size,
                        "ready_for_execute"
                    );
                }
                drain_exec(&exec, &undo_path);
                if !ready.is_empty() && !paused.load(Ordering::Relaxed) {
                    let cfg_now = cfg_snapshot(&cfg);
                    let folders_now = folders_snapshot(&folders);
                    let ctx = ReadyCtx {
                        cfg: &cfg_now,
                        folders: &folders_now,
                        candidates: &candidates,
                        live: &state,
                        undo_path: &undo_path,
                        state_path: &state_path,
                        archived_today: &archived_today,
                    };
                    execute_ready(
                        &ctx,
                        &ready,
                        &mut limiter,
                        &mut illegal_warned,
                        &mut persist_retry,
                    );
                }
                thread::sleep(tick);
            }
        })
        .expect("stabilize thread")
}

fn cfg_snapshot(cfg: &Mutex<Config>) -> Config {
    cfg.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn folders_snapshot(folders: &Mutex<KnownFolders>) -> KnownFolders {
    folders.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn drain_exec(exec: &Receiver<ExecCmd>, undo_path: &Path) {
    loop {
        match exec.try_recv() {
            Ok(ExecCmd::UndoLast { reply }) => {
                let _ = reply.send(undo_last(undo_path));
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
        }
    }
}

pub fn tick_once(
    candidates: &Mutex<HashMap<PathBuf, Candidate>>,
    ignore: &IgnoreSet,
    now: SystemTime,
    paused: bool,
) -> Vec<FileSnapshot> {
    tick_once_with(candidates, ignore, now, paused, None, None)
}

fn tick_once_with(
    candidates: &Mutex<HashMap<PathBuf, Candidate>>,
    ignore: &IgnoreSet,
    now: SystemTime,
    paused: bool,
    live: Option<&Mutex<AppState>>,
    persist_retry: Option<&mut bool>,
) -> Vec<FileSnapshot> {
    let blocked: Vec<(PathBuf, PathBuf)> = live
        .map(|s| {
            s.lock()
                .unwrap_or_else(|e| e.into_inner())
                .blocked
                .iter()
                .map(|b| (b.from.clone(), b.to.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut unblocked = Vec::new();
    let ready = {
        let mut table = candidates.lock().unwrap_or_else(|e| e.into_inner());
        let paths: Vec<PathBuf> = table.keys().cloned().collect();
        let mut drop_list = Vec::new();
        let mut ready = Vec::new();

        for path in paths {
            let Some(cand) = table.get(&path) else {
                continue;
            };
            let poisoned = cand.poisoned;
            match inspect_file(&path) {
                Ok(info) if should_keep(&path, &info, ignore) => {
                    if poisoned {
                        let dest = blocked
                            .iter()
                            .find(|(from, _)| crate::state::same_path(from, &path))
                            .map(|(_, to)| to);
                        if dest.is_some_and(|to| !to.exists()) {
                            if let Some(c) = table.get_mut(&path) {
                                c.poisoned = false;
                            }
                            unblocked.push(path.clone());
                            sample(&mut table, &path, &info, now, paused, &mut ready);
                        }
                        continue;
                    }
                    sample(&mut table, &path, &info, now, paused, &mut ready);
                }
                _ => drop_list.push(path),
            }
        }

        for path in drop_list {
            table.remove(&path);
        }
        ready
    };

    if !unblocked.is_empty() {
        if let Some(live) = live {
            let mut state = live.lock().unwrap_or_else(|e| e.into_inner());
            for path in &unblocked {
                state.remove_blocked_from(path);
            }
        }
        if let Some(retry) = persist_retry {
            *retry = true;
        }
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
    if cand.stable_since.is_none() {
        cand.stable_since = Some(now);
    }
    if cand.attempts > 0 && !backoff_elapsed(cand, now) {
        return;
    }
    if cand.attempts >= MAX_ATTEMPTS {
        cand.attempts = 0;
        cand.last_error_at = None;
    }
    if paused || !cand.is_ready(now) {
        return;
    }
    ready.push(cand.snapshot(path.to_path_buf()));
}

fn retry_backoff(attempts: u32) -> Duration {
    let i = attempts.saturating_sub(1) as usize;
    Duration::from_secs(*RETRY_BACKOFF_SECS.get(i).unwrap_or(&60))
}

fn backoff_elapsed(cand: &Candidate, now: SystemTime) -> bool {
    let Some(at) = cand.last_error_at else {
        return true;
    };
    now.duration_since(at)
        .map(|d| d >= retry_backoff(cand.attempts))
        .unwrap_or(true)
}

fn flush_persist(live: &Mutex<AppState>, path: &Path, retry: &mut bool) {
    if !*retry {
        return;
    }
    match live.lock().unwrap_or_else(|e| e.into_inner()).save(path) {
        Ok(()) => *retry = false,
        Err(err) => tracing::error!(error = %err, "persist blocked failed"),
    }
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

struct ReadyCtx<'a> {
    cfg: &'a Config,
    folders: &'a KnownFolders,
    candidates: &'a Mutex<HashMap<PathBuf, Candidate>>,
    live: &'a Mutex<AppState>,
    undo_path: &'a Path,
    state_path: &'a Path,
    archived_today: &'a AtomicU32,
}

enum Outcome {
    Moved,
    Classify(ClassifyError),
    Exec(ExecError),
}

enum Next {
    Continue,
    Stop,
}

#[derive(Default)]
struct LogLimiter {
    last: HashMap<&'static str, Instant>,
}

impl LogLimiter {
    fn allow(&mut self, key: &'static str) -> bool {
        let now = Instant::now();
        if self
            .last
            .get(&key)
            .is_some_and(|prev| now.saturating_duration_since(*prev) < ERROR_EVERY)
        {
            return false;
        }
        self.last.insert(key, now);
        true
    }
}

fn execute_ready(
    ctx: &ReadyCtx<'_>,
    ready: &[FileSnapshot],
    limiter: &mut LogLimiter,
    illegal_warned: &mut HashSet<PathBuf>,
    persist_retry: &mut bool,
) {
    for snap in ready {
        // classify + Move run with no candidate lock (T1 may upsert during a cross-volume copy).
        let outcome = match classify(ctx.cfg, snap, ctx.folders) {
            Err(err) => Outcome::Classify(err),
            Ok(placement) => match execute_move(
                ctx.cfg,
                snap,
                &placement,
                ctx.undo_path,
                ctx.live,
                ctx.state_path,
            ) {
                Ok(_) => Outcome::Moved,
                Err(err) => Outcome::Exec(err),
            },
        };
        if matches!(
            apply_outcome(
                ctx,
                &snap.path,
                outcome,
                limiter,
                illegal_warned,
                persist_retry
            ),
            Next::Stop
        ) {
            break;
        }
    }
}

fn apply_outcome(
    ctx: &ReadyCtx<'_>,
    path: &Path,
    outcome: Outcome,
    limiter: &mut LogLimiter,
    illegal_warned: &mut HashSet<PathBuf>,
    persist_retry: &mut bool,
) -> Next {
    match outcome {
        Outcome::Moved => {
            ctx.archived_today.fetch_add(1, Ordering::Relaxed);
            remove_candidate(ctx.candidates, path);
            Next::Continue
        }
        Outcome::Classify(ClassifyError::BadTemplate) => {
            if limiter.allow("classify_bad_template") {
                tracing::error!(path = %path.display(), "bad destination template");
            }
            remove_candidate(ctx.candidates, path);
            Next::Continue
        }
        Outcome::Classify(ClassifyError::DestInsideSource) => {
            if limiter.allow("classify_dest_inside") {
                tracing::error!(path = %path.display(), "destination inside source or denylist");
            }
            poison_candidate(ctx.candidates, path);
            Next::Continue
        }
        Outcome::Classify(ClassifyError::IllegalName) => {
            if illegal_warned.insert(path.to_path_buf()) {
                tracing::warn!(path = %path.display(), "illegal file name");
            }
            remove_candidate(ctx.candidates, path);
            Next::Continue
        }
        Outcome::Exec(ExecError::Skipped) => {
            {
                let mut live = ctx.live.lock().unwrap_or_else(|e| e.into_inner());
                live.push_skipped(path.to_path_buf());
            }
            remove_candidate(ctx.candidates, path);
            Next::Continue
        }
        Outcome::Exec(ExecError::SplitCopy { to }) => {
            {
                let mut live = ctx.live.lock().unwrap_or_else(|e| e.into_inner());
                live.push_blocked(path.to_path_buf(), to);
                if live.save(ctx.state_path).is_err() {
                    *persist_retry = true;
                }
            }
            if limiter.allow("exec_poison") {
                tracing::error!(path = %path.display(), "move poisoned");
            }
            poison_candidate(ctx.candidates, path);
            Next::Continue
        }
        Outcome::Exec(ExecError::Blocked)
        | Outcome::Exec(ExecError::SamePath)
        | Outcome::Exec(ExecError::CollisionLimit) => {
            if limiter.allow("exec_poison") {
                tracing::error!(path = %path.display(), "move poisoned");
            }
            poison_candidate(ctx.candidates, path);
            Next::Continue
        }
        Outcome::Exec(ExecError::DiskFull) => {
            if limiter.allow("exec_disk_full") {
                tracing::error!(path = %path.display(), "disk full");
            }
            Next::Stop
        }
        Outcome::Exec(ExecError::SharingViolation)
        | Outcome::Exec(ExecError::AccessDenied)
        | Outcome::Exec(ExecError::CopyLeftSource)
        | Outcome::Exec(ExecError::Io(_)) => {
            let attempts = bump_attempts(ctx.candidates, path, SystemTime::now());
            if attempts >= MAX_ATTEMPTS && limiter.allow("exec_retry") {
                tracing::error!(path = %path.display(), attempts, "move failed");
            }
            Next::Continue
        }
    }
}

fn remove_candidate(table: &Mutex<HashMap<PathBuf, Candidate>>, path: &Path) {
    table.lock().unwrap_or_else(|e| e.into_inner()).remove(path);
}

fn poison_candidate(table: &Mutex<HashMap<PathBuf, Candidate>>, path: &Path) {
    if let Some(cand) = table
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(path)
    {
        cand.poisoned = true;
    }
}

fn bump_attempts(table: &Mutex<HashMap<PathBuf, Candidate>>, path: &Path, now: SystemTime) -> u32 {
    let mut table = table.lock().unwrap_or_else(|e| e.into_inner());
    match table.get_mut(path) {
        Some(cand) => {
            cand.attempts = cand.attempts.saturating_add(1);
            cand.last_error_at = Some(now);
            cand.attempts
        }
        None => 0,
    }
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
    #[cfg(windows)]
    use crate::config::Collision;
    use crate::config::SourceKind;
    #[cfg(windows)]
    use crate::undo::{read_records, JournalOp};
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
            last_error_at: None,
            poisoned: false,
            settle_secs,
        }
    }

    fn cand_ready() -> Candidate {
        Candidate {
            source_id: "downloads".into(),
            source_kind: SourceKind::Downloads,
            first_seen: SystemTime::now(),
            last_size: 1,
            last_mtime: UNIX_EPOCH,
            created: UNIX_EPOCH,
            stable_since: Some(UNIX_EPOCH),
            attempts: 0,
            last_error_at: None,
            poisoned: false,
            settle_secs: 0,
        }
    }

    fn win_folders() -> KnownFolders {
        KnownFolders {
            downloads: Some(PathBuf::from(r"C:\Users\a\Downloads")),
            screenshots: Some(PathBuf::from(r"C:\Users\a\Pictures\Screenshots")),
            documents: Some(PathBuf::from(r"C:\Users\a\Documents")),
            profile: Some(PathBuf::from(r"C:\Users\a")),
            windows: Some(PathBuf::from(r"C:\Windows")),
            program_files: vec![
                PathBuf::from(r"C:\Program Files"),
                PathBuf::from(r"C:\Program Files (x86)"),
            ],
            program_data: Some(PathBuf::from(r"C:\ProgramData")),
            ..KnownFolders::default()
        }
    }

    fn snap_at(path: PathBuf) -> FileSnapshot {
        FileSnapshot {
            path,
            source_id: "downloads".into(),
            source_kind: SourceKind::Downloads,
            size: 1,
            mtime: UNIX_EPOCH,
            created: UNIX_EPOCH,
        }
    }

    fn run_ready(
        cfg: &Config,
        folders: &KnownFolders,
        table: &Mutex<HashMap<PathBuf, Candidate>>,
        live: &Mutex<AppState>,
        dir: &Path,
        ready: &[FileSnapshot],
    ) {
        let undo = dir.join("undo.jsonl");
        let state_path = dir.join("state.json");
        let archived_today = AtomicU32::new(0);
        let ctx = ReadyCtx {
            cfg,
            folders,
            candidates: table,
            live,
            undo_path: &undo,
            state_path: &state_path,
            archived_today: &archived_today,
        };
        let mut limiter = LogLimiter::default();
        let mut illegal = HashSet::new();
        let mut persist_retry = false;
        execute_ready(&ctx, ready, &mut limiter, &mut illegal, &mut persist_retry);
    }

    fn apply(
        table: &Mutex<HashMap<PathBuf, Candidate>>,
        live: &Mutex<AppState>,
        dir: &Path,
        path: &Path,
        outcome: Outcome,
    ) -> Next {
        let cfg = Config::default();
        let folders = KnownFolders::default();
        let undo = dir.join("undo.jsonl");
        let state_path = dir.join("state.json");
        let archived_today = AtomicU32::new(0);
        let ctx = ReadyCtx {
            cfg: &cfg,
            folders: &folders,
            candidates: table,
            live,
            undo_path: &undo,
            state_path: &state_path,
            archived_today: &archived_today,
        };
        let mut limiter = LogLimiter::default();
        let mut illegal = HashSet::new();
        let mut persist_retry = false;
        apply_outcome(
            &ctx,
            path,
            outcome,
            &mut limiter,
            &mut illegal,
            &mut persist_retry,
        )
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

    #[test]
    fn execute_failure_backoff_skips_ready() {
        let dir = temp_dir();
        let (path, info) = insert_file(&dir, "a.pdf", b"x");
        let mut cand = candidate(&info, 0);
        cand.attempts = 1;
        cand.last_error_at = Some(SystemTime::now());
        cand.stable_since = Some(UNIX_EPOCH);
        let table = Mutex::new(HashMap::from([(path.clone(), cand)]));
        let ready = tick_once(&table, &ignore(), SystemTime::now(), false);
        assert!(ready.is_empty());
        assert_eq!(table.lock().unwrap()[&path].attempts, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn backoff_elapsed_keeps_attempts_under_cap() {
        let dir = temp_dir();
        let (path, info) = insert_file(&dir, "a.pdf", b"x");
        let now = SystemTime::now();
        let mut cand = candidate(&info, 0);
        cand.attempts = 2;
        cand.last_error_at = Some(now - Duration::from_secs(16));
        cand.stable_since = Some(UNIX_EPOCH);
        let table = Mutex::new(HashMap::from([(path.clone(), cand)]));
        let ready = tick_once(&table, &ignore(), now, false);
        assert_eq!(ready.len(), 1);
        assert_eq!(table.lock().unwrap()[&path].attempts, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn attempts_cap_zeros_after_backoff() {
        let dir = temp_dir();
        let (path, info) = insert_file(&dir, "a.pdf", b"x");
        let now = SystemTime::now();
        let mut cand = candidate(&info, 0);
        cand.attempts = MAX_ATTEMPTS;
        cand.last_error_at = Some(now - Duration::from_secs(61));
        cand.stable_since = Some(UNIX_EPOCH);
        let table = Mutex::new(HashMap::from([(path.clone(), cand)]));
        let ready = tick_once(&table, &ignore(), now, false);
        assert_eq!(ready.len(), 1);
        assert_eq!(table.lock().unwrap()[&path].attempts, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_template_drops_candidate() {
        let dir = temp_dir();
        let path = PathBuf::from(r"C:\Users\a\Downloads\a.pdf");
        let table = Mutex::new(HashMap::from([(path.clone(), cand_ready())]));
        let mut cfg = Config::default();
        cfg.destination.root = Some(r"C:\Users\a\Documents\Weekcase".into());
        cfg.destination.downloads_template = "{root}/{ww}".into();
        let live = Mutex::new(AppState::default());
        run_ready(
            &cfg,
            &win_folders(),
            &table,
            &live,
            &dir,
            &[snap_at(path.clone())],
        );
        assert!(table.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dest_inside_source_poisons() {
        let dir = temp_dir();
        let path = PathBuf::from(r"C:\Users\a\Downloads\a.pdf");
        let table = Mutex::new(HashMap::from([(path.clone(), cand_ready())]));
        let mut cfg = Config::default();
        cfg.destination.root = Some(r"C:\Users\a\Downloads\Weekcase".into());
        let live = Mutex::new(AppState::default());
        run_ready(
            &cfg,
            &win_folders(),
            &table,
            &live,
            &dir,
            &[snap_at(path.clone())],
        );
        let t = table.lock().unwrap();
        assert!(t[&path].poisoned);
        assert_eq!(t.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn illegal_name_drops_once() {
        let dir = temp_dir();
        let path = PathBuf::from(r"C:\Users\a\Downloads\..");
        let table = Mutex::new(HashMap::from([(path.clone(), cand_ready())]));
        let mut cfg = Config::default();
        cfg.destination.root = Some(r"C:\Users\a\Documents\Weekcase".into());
        let live = Mutex::new(AppState::default());
        run_ready(
            &cfg,
            &win_folders(),
            &table,
            &live,
            &dir,
            &[snap_at(path.clone())],
        );
        assert!(table.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sharing_increments_attempts() {
        let dir = temp_dir();
        let path = PathBuf::from(r"C:\Users\a\Downloads\a.pdf");
        let table = Mutex::new(HashMap::from([(path.clone(), cand_ready())]));
        let live = Mutex::new(AppState::default());
        let next = apply(
            &table,
            &live,
            &dir,
            &path,
            Outcome::Exec(ExecError::SharingViolation),
        );
        assert!(matches!(next, Next::Continue));
        let t = table.lock().unwrap();
        assert_eq!(t[&path].attempts, 1);
        assert!(t[&path].last_error_at.is_some());
        assert!(!t[&path].poisoned);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fifth_sharing_reaches_max_attempts() {
        let dir = temp_dir();
        let path = PathBuf::from(r"C:\Users\a\Downloads\a.pdf");
        let table = Mutex::new(HashMap::from([(path.clone(), cand_ready())]));
        let live = Mutex::new(AppState::default());
        for n in 1..=MAX_ATTEMPTS {
            apply(
                &table,
                &live,
                &dir,
                &path,
                Outcome::Exec(ExecError::SharingViolation),
            );
            assert_eq!(table.lock().unwrap()[&path].attempts, n);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disk_full_stops_without_burning_attempts() {
        let dir = temp_dir();
        let path = PathBuf::from(r"C:\Users\a\Downloads\a.pdf");
        let table = Mutex::new(HashMap::from([(path.clone(), cand_ready())]));
        let live = Mutex::new(AppState::default());
        let next = apply(
            &table,
            &live,
            &dir,
            &path,
            Outcome::Exec(ExecError::DiskFull),
        );
        assert!(matches!(next, Next::Stop));
        assert_eq!(table.lock().unwrap()[&path].attempts, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skipped_removes_and_records_live_state() {
        let dir = temp_dir();
        let path = PathBuf::from(r"C:\Users\a\Downloads\a.pdf");
        let table = Mutex::new(HashMap::from([(path.clone(), cand_ready())]));
        let live = Mutex::new(AppState::default());
        apply(
            &table,
            &live,
            &dir,
            &path,
            Outcome::Exec(ExecError::Skipped),
        );
        assert!(table.lock().unwrap().is_empty());
        assert!(live.lock().unwrap().is_skipped(&path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_copy_poisons() {
        let dir = temp_dir();
        let path = PathBuf::from(r"C:\Users\a\Downloads\a.pdf");
        let table = Mutex::new(HashMap::from([(path.clone(), cand_ready())]));
        let live = Mutex::new(AppState::default());
        let dest = PathBuf::from(r"C:\out\a.pdf");
        apply(
            &table,
            &live,
            &dir,
            &path,
            Outcome::Exec(ExecError::SplitCopy { to: dest.clone() }),
        );
        assert!(table.lock().unwrap()[&path].poisoned);
        let st = live.lock().unwrap();
        assert!(st.is_blocked_from(&path));
        assert_eq!(st.blocked[0].to, dest);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn poisoned_missing_is_dropped() {
        let dir = temp_dir();
        let missing = dir.join("gone.pdf");
        let mut cand = cand_ready();
        cand.poisoned = true;
        let table = Mutex::new(HashMap::from([(missing, cand)]));
        let ready = tick_once(&table, &ignore(), SystemTime::now(), false);
        assert!(ready.is_empty());
        assert!(table.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dest_gone_unpoisons_blocked() {
        let dir = temp_dir();
        let (path, info) = insert_file(&dir, "a.pdf", b"x");
        let dest = dir.join("leftover.bin");
        fs::write(&dest, b"copy").unwrap();
        let mut cand = candidate(&info, 0);
        cand.poisoned = true;
        cand.stable_since = Some(UNIX_EPOCH);
        let table = Mutex::new(HashMap::from([(path.clone(), cand)]));
        let live = Mutex::new(AppState::default());
        live.lock()
            .unwrap()
            .push_blocked(path.clone(), dest.clone());
        let ready = tick_once_with(
            &table,
            &ignore(),
            SystemTime::now(),
            false,
            Some(&live),
            None,
        );
        assert!(ready.is_empty());
        assert!(table.lock().unwrap()[&path].poisoned);

        fs::remove_file(&dest).unwrap();
        let mut persist_retry = false;
        let ready = tick_once_with(
            &table,
            &ignore(),
            SystemTime::now(),
            false,
            Some(&live),
            Some(&mut persist_retry),
        );
        assert_eq!(ready.len(), 1);
        assert!(!table.lock().unwrap()[&path].poisoned);
        assert!(!live.lock().unwrap().is_blocked_from(&path));
        assert!(persist_retry);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn moved_removes_candidate() {
        let dir = temp_dir();
        let path = PathBuf::from(r"C:\Users\a\Downloads\a.pdf");
        let table = Mutex::new(HashMap::from([(path.clone(), cand_ready())]));
        let live = Mutex::new(AppState::default());
        apply(&table, &live, &dir, &path, Outcome::Moved);
        assert!(table.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn pipeline_classifies_moves_and_appends_undo() {
        let dir = temp_dir();
        let src = dir.join("src");
        let archive = dir.join("archive");
        fs::create_dir_all(&src).unwrap();
        let (path, info) = insert_file(&src, "a.pdf", b"hello");
        let table = Mutex::new(HashMap::from([(path.clone(), candidate(&info, 0))]));
        let mut cfg = Config::default();
        cfg.destination.root = Some(archive.to_string_lossy().into_owned());
        cfg.sources[0].path = Some(src.to_string_lossy().into_owned());
        let live = Mutex::new(AppState::default());
        let ready = tick_once(&table, &ignore(), SystemTime::now(), false);
        assert_eq!(ready.len(), 1);
        run_ready(&cfg, &KnownFolders::default(), &table, &live, &dir, &ready);
        assert!(!path.exists());
        let dest = archive.join("Downloads").join("Documents").join("a.pdf");
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
        assert!(table.lock().unwrap().is_empty());
        let recs = read_records(&dir.join("undo.jsonl")).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].op, JournalOp::Move);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn pipeline_skip_collision_removes_without_move() {
        let dir = temp_dir();
        let src = dir.join("src");
        let archive = dir.join("archive");
        let dest_dir = archive.join("Downloads").join("Documents");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest_dir).unwrap();
        let (path, info) = insert_file(&src, "a.pdf", b"new");
        fs::write(dest_dir.join("a.pdf"), b"old").unwrap();
        let table = Mutex::new(HashMap::from([(path.clone(), candidate(&info, 0))]));
        let mut cfg = Config::default();
        cfg.destination.root = Some(archive.to_string_lossy().into_owned());
        cfg.destination.collision = Collision::Skip;
        cfg.sources[0].path = Some(src.to_string_lossy().into_owned());
        let live = Mutex::new(AppState::default());
        let ready = tick_once(&table, &ignore(), SystemTime::now(), false);
        run_ready(&cfg, &KnownFolders::default(), &table, &live, &dir, &ready);
        assert!(path.exists());
        assert_eq!(fs::read(dest_dir.join("a.pdf")).unwrap(), b"old");
        assert!(table.lock().unwrap().is_empty());
        assert!(live.lock().unwrap().is_skipped(&path));
        let _ = fs::remove_dir_all(&dir);
    }
}

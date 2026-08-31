use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::candidate::{clear_poisoned, upsert, Upsert};
use crate::config::{Config, SourceConfig};
use crate::known_folders::{canonical_key, deny_source, is_same_or_inside, KnownFolders};
use crate::paths::Paths;
use crate::state::AppState;

pub type SourceId = String;
pub use crate::candidate::{Candidate, FileSnapshot};

const MAX_PATH_BYTES: usize = 32 * 1024;
const POLL: Duration = Duration::from_millis(100);
const MISSING_RETRY: Duration = Duration::from_secs(60);
const OPEN_BACKOFF_SECS: &[u64] = &[1, 5, 15, 60];
const IGNORED_EXTS: &[&str] = &[
    "crdownload",
    "part",
    "partial",
    "tmp",
    "temp",
    "download",
    "opdownload",
    "!ut",
    "bc!",
    "filepart",
    "!qb",
    "qbmd",
    "aria2",
    "ytdl",
];
const IGNORED_NAMES: &[&str] = &["desktop.ini", "thumbs.db", ".ds_store"];

pub enum WatchCmd {
    Rescan {
        source: Option<SourceId>,
        include_existing: bool,
        min_age_override: Option<Duration>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Default)]
pub struct IgnoreSet {
    pub exe: Option<PathBuf>,
    pub prefixes: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
}

impl IgnoreSet {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_process() -> Self {
        match Paths::resolve() {
            Ok(paths) => Self::from_paths(&paths),
            Err(_) => Self {
                exe: std::env::current_exe().ok(),
                ..Self::default()
            },
        }
    }

    pub fn from_paths(paths: &Paths) -> Self {
        let exe = std::env::current_exe()
            .ok()
            .map(|p| fs::canonicalize(&p).unwrap_or(p));
        let mut prefixes = vec![paths.logs_dir()];
        if paths.portable {
            prefixes.push(paths.config_dir.clone());
        }
        Self {
            exe,
            prefixes,
            files: vec![paths.state_file(), paths.undo_file(), paths.log_file()],
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub size: u64,
    pub mtime: SystemTime,
    pub created: SystemTime,
    pub is_file: bool,
    pub placeholder: bool,
}

#[derive(Debug, Clone)]
pub struct AdmitPolicy {
    pub min_age: Duration,
    pub include_existing: bool,
    pub first_run_at: Option<SystemTime>,
}

pub fn start_watch(
    cfg: Arc<Config>,
    state: Arc<Mutex<AppState>>,
    candidates: Arc<Mutex<HashMap<PathBuf, Candidate>>>,
    cmd: Receiver<WatchCmd>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("weekcase-watch".into())
        .spawn(move || run_watch(cfg, state, candidates, cmd))
        .expect("watch thread")
}

pub fn is_ignored(path: &Path, ignore: &IgnoreSet) -> bool {
    if let Some(name) = path.file_name().map(|n| n.to_string_lossy()) {
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("~$") || IGNORED_NAMES.contains(&lower.as_str()) {
            return true;
        }
        if let Some((stem, ext)) = lower.rsplit_once('.') {
            if !stem.is_empty() && IGNORED_EXTS.contains(&ext) {
                return true;
            }
        }
    }
    if ignore
        .exe
        .as_ref()
        .is_some_and(|exe| canonical_key(exe) == canonical_key(path))
    {
        return true;
    }
    if ignore
        .files
        .iter()
        .any(|f| canonical_key(f) == canonical_key(path))
    {
        return true;
    }
    ignore.prefixes.iter().any(|p| is_same_or_inside(path, p))
}

pub fn inspect_file(path: &Path) -> io::Result<FileInfo> {
    #[cfg(windows)]
    {
        inspect_windows(path)
    }
    #[cfg(not(windows))]
    {
        inspect_unix(path)
    }
}

pub fn is_top_level_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

pub fn join_top_level(dir: &Path, name: &str) -> Option<PathBuf> {
    if !is_top_level_name(name) {
        return None;
    }
    normalize_path(&dir.join(name))
}

pub fn normalize_path(path: &Path) -> Option<PathBuf> {
    if path.as_os_str().len() > MAX_PATH_BYTES || path.as_os_str().is_empty() {
        return None;
    }
    #[cfg(windows)]
    {
        normalize_windows(path)
    }
    #[cfg(not(windows))]
    {
        Some(path.to_path_buf())
    }
}

#[allow(clippy::too_many_arguments)]
pub fn collect_admits(
    dir: &Path,
    source: &SourceConfig,
    policy: &AdmitPolicy,
    ignore: &IgnoreSet,
    state: &AppState,
    now: SystemTime,
) -> io::Result<Vec<(PathBuf, Candidate)>> {
    let mut out = Vec::new();
    for ent in fs::read_dir(dir)? {
        let Ok(ent) = ent else {
            continue;
        };
        let name = ent.file_name();
        let Some(path) = join_top_level(dir, &name.to_string_lossy()) else {
            continue;
        };
        if let Some(cand) = consider_path(&path, source, policy, ignore, state, now) {
            out.push((path, cand));
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
pub fn scan_dir(
    dir: &Path,
    source: &SourceConfig,
    policy: &AdmitPolicy,
    ignore: &IgnoreSet,
    state: &AppState,
    table: &mut HashMap<PathBuf, Candidate>,
    max_pending: usize,
    now: SystemTime,
) -> bool {
    let Ok(admits) = collect_admits(dir, source, policy, ignore, state, now) else {
        return false;
    };
    let mut overflow = false;
    for (path, cand) in admits {
        match upsert(table, path, cand, max_pending, now) {
            Upsert::Rejected | Upsert::ReplacedOldest => overflow = true,
            _ => {}
        }
    }
    overflow
}

pub fn consider_path(
    path: &Path,
    source: &SourceConfig,
    policy: &AdmitPolicy,
    ignore: &IgnoreSet,
    state: &AppState,
    now: SystemTime,
) -> Option<Candidate> {
    let info = inspect_file(path).ok()?;
    admit(path, &info, source, policy, ignore, state, now)
}

pub fn admit(
    path: &Path,
    info: &FileInfo,
    source: &SourceConfig,
    policy: &AdmitPolicy,
    ignore: &IgnoreSet,
    state: &AppState,
    now: SystemTime,
) -> Option<Candidate> {
    if !info.is_file || info.placeholder || is_ignored(path, ignore) {
        return None;
    }
    if state.is_blocked_from(path) {
        return None;
    }
    if !policy.include_existing && state.is_skipped(path) {
        return None;
    }
    if !policy.include_existing {
        let first = policy.first_run_at?;
        if info.created < first {
            return None;
        }
    }
    if now
        .duration_since(info.created)
        .map(|d| d < policy.min_age)
        .unwrap_or(true)
    {
        return None;
    }
    Some(Candidate {
        source_id: source.id.clone(),
        source_kind: source.kind,
        first_seen: now,
        last_size: info.size,
        last_mtime: info.mtime,
        created: info.created,
        stable_since: None,
        attempts: 0,
        last_error_at: None,
        poisoned: false,
        settle_secs: source.settle_secs,
    })
}

fn run_watch(
    cfg: Arc<Config>,
    state: Arc<Mutex<AppState>>,
    candidates: Arc<Mutex<HashMap<PathBuf, Candidate>>>,
    cmd: Receiver<WatchCmd>,
) {
    let folders = KnownFolders::resolve();
    let ignore = IgnoreSet::from_process();
    let mut rt = WatchRuntime::new(cfg, state, candidates, cmd, folders, ignore);
    rt.boot();
    rt.loop_until_shutdown();
}

struct WatchRuntime {
    state: Arc<Mutex<AppState>>,
    candidates: Arc<Mutex<HashMap<PathBuf, Candidate>>>,
    folders: KnownFolders,
    ignore: IgnoreSet,
    sources: Vec<SourceSlot>,
    pending: HashMap<PathBuf, (usize, Instant)>,
    debounce: Duration,
    max_pending: usize,
    #[cfg(windows)]
    buf_len: usize,
    cmd: Receiver<WatchCmd>,
    #[cfg(windows)]
    iocp: Option<windows::Win32::Foundation::HANDLE>,
    #[cfg(windows)]
    shutting_down: bool,
}

struct SourceSlot {
    cfg: SourceConfig,
    resolved: Option<PathBuf>,
    watch_path: Option<PathBuf>,
    next_scan: Instant,
    next_open: Instant,
    backoff_i: usize,
    open: bool,
    dead: bool,
    #[cfg(windows)]
    rdc: Option<RdcState>,
    #[cfg(windows)]
    rdc_fails: u32,
    #[cfg(windows)]
    rdc_closing: bool,
    #[cfg(windows)]
    reopen_after_close: bool,
}

#[cfg(windows)]
struct RdcState {
    handle: windows::Win32::Foundation::HANDLE,
    overlapped: Box<windows::Win32::System::IO::OVERLAPPED>,
    buffer: Vec<u8>,
    pending: bool,
}

impl Drop for WatchRuntime {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            self.shutting_down = true;
            for i in 0..self.sources.len() {
                self.begin_close(i, false);
            }
            self.drain_cancels();
            for slot in &mut self.sources {
                slot.rdc = None;
            }
            if let Some(port) = self.iocp.take() {
                if !port.is_invalid() {
                    unsafe {
                        let _ = windows::Win32::Foundation::CloseHandle(port);
                    }
                }
            }
        }
        #[cfg(not(windows))]
        {
            for slot in &mut self.sources {
                slot.open = false;
            }
        }
    }
}

#[cfg(windows)]
impl Drop for RdcState {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::IO::OVERLAPPED;
        if self.pending {
            // A completion may still write these; leak rather than UAF.
            let ov = std::mem::replace(&mut self.overlapped, Box::new(OVERLAPPED::default()));
            let buf = std::mem::take(&mut self.buffer);
            std::mem::forget(ov);
            std::mem::forget(buf);
            self.pending = false;
        }
        if !self.handle.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
            self.handle = HANDLE::default();
        }
    }
}

impl WatchRuntime {
    fn new(
        cfg: Arc<Config>,
        state: Arc<Mutex<AppState>>,
        candidates: Arc<Mutex<HashMap<PathBuf, Candidate>>>,
        cmd: Receiver<WatchCmd>,
        folders: KnownFolders,
        ignore: IgnoreSet,
    ) -> Self {
        let max_sources = cfg.watch.max_sources as usize;
        let mut sources = Vec::new();
        for src in cfg.sources.iter().filter(|s| s.enabled) {
            if sources.len() >= max_sources {
                break;
            }
            sources.push(SourceSlot::from_config(src, &folders));
        }
        #[cfg(windows)]
        let buf_len = if cfg.watch.buffer_bytes == 0 {
            64 * 1024
        } else {
            cfg.watch.buffer_bytes as usize
        };
        Self {
            debounce: Duration::from_millis(cfg.watch.debounce_ms),
            max_pending: cfg.watch.max_pending as usize,
            #[cfg(windows)]
            buf_len,
            state,
            candidates,
            folders,
            ignore,
            sources,
            pending: HashMap::new(),
            cmd,
            #[cfg(windows)]
            iocp: create_iocp(),
            #[cfg(windows)]
            shutting_down: false,
        }
    }

    fn boot(&mut self) {
        for i in 0..self.sources.len() {
            if self.sources[i].dead {
                continue;
            }
            self.open_source(i);
            if self.sources[i].open {
                self.scan_one(i, false, None);
            }
        }
    }

    fn loop_until_shutdown(&mut self) {
        loop {
            if self.drain_cmds() {
                break;
            }
            self.reopen_due();
            self.scan_due();
            self.flush_debounce();
            if self.wait_idle() {
                break;
            }
        }
    }

    fn drain_cmds(&mut self) -> bool {
        loop {
            match self.cmd.try_recv() {
                Ok(WatchCmd::Shutdown) | Err(TryRecvError::Disconnected) => return true,
                Ok(WatchCmd::Rescan {
                    source,
                    include_existing,
                    min_age_override,
                }) => self.rescan(source.as_deref(), include_existing, min_age_override),
                Err(TryRecvError::Empty) => return false,
            }
        }
    }

    fn wait_idle(&mut self) -> bool {
        #[cfg(windows)]
        {
            self.wait_iocp(POLL)
        }
        #[cfg(not(windows))]
        {
            match self.cmd.recv_timeout(POLL) {
                Ok(WatchCmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => true,
                Ok(WatchCmd::Rescan {
                    source,
                    include_existing,
                    min_age_override,
                }) => {
                    self.rescan(source.as_deref(), include_existing, min_age_override);
                    false
                }
                Err(RecvTimeoutError::Timeout) => false,
            }
        }
    }

    fn rescan(
        &mut self,
        source: Option<&str>,
        include_existing: bool,
        min_age_override: Option<Duration>,
    ) {
        {
            let mut table = self.candidates.lock().unwrap_or_else(|e| e.into_inner());
            clear_poisoned(&mut table, source);
        }
        for i in 0..self.sources.len() {
            if source.is_some_and(|id| self.sources[i].cfg.id != id) {
                continue;
            }
            if self.sources[i].dead {
                continue;
            }
            if !self.sources[i].open {
                self.open_source(i);
            }
            if self.sources[i].open {
                self.scan_one(i, include_existing, min_age_override);
            }
        }
    }

    fn reopen_due(&mut self) {
        let now = Instant::now();
        for i in 0..self.sources.len() {
            if self.sources[i].dead || self.sources[i].open {
                continue;
            }
            if now >= self.sources[i].next_open {
                self.open_source(i);
                if self.sources[i].open {
                    self.scan_one(i, false, None);
                }
            }
        }
    }

    fn scan_due(&mut self) {
        let now = Instant::now();
        for i in 0..self.sources.len() {
            if self.sources[i].open && now >= self.sources[i].next_scan {
                self.scan_one(i, false, None);
            }
        }
    }

    fn scan_one(&mut self, i: usize, include_existing: bool, min_age_override: Option<Duration>) {
        let Some(dir) = self.sources[i].watch_path.clone() else {
            return;
        };
        let policy = self.policy(i, include_existing, min_age_override);
        let snap = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let src = self.sources[i].cfg.clone();
        let now = SystemTime::now();
        match collect_admits(&dir, &src, &policy, &self.ignore, &snap, now) {
            Err(err) => {
                tracing::error!(
                    id = %src.id,
                    path = %dir.display(),
                    %err,
                    "source directory lost"
                );
                self.source_lost(i);
            }
            Ok(admits) => {
                self.sources[i].next_scan =
                    Instant::now() + Duration::from_secs(src.scan_interval_secs);
                for (path, cand) in admits {
                    self.upsert_candidate(i, path, cand);
                }
            }
        }
    }

    fn policy(
        &self,
        i: usize,
        include_existing: bool,
        min_age_override: Option<Duration>,
    ) -> AdmitPolicy {
        let first_run_at = first_run_from(&self.state.lock().unwrap_or_else(|e| e.into_inner()));
        AdmitPolicy {
            min_age: min_age_override
                .unwrap_or_else(|| Duration::from_secs(self.sources[i].cfg.min_age_secs)),
            include_existing,
            first_run_at,
        }
    }

    fn note_overflow(&self, source: &str) {
        tracing::warn!(source, "pending_overflow");
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .overflow_unacked = true;
    }

    fn flush_debounce(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let now = Instant::now();
        let due: Vec<(PathBuf, usize)> = self
            .pending
            .iter()
            .filter(|(_, (_, t))| now.saturating_duration_since(*t) >= self.debounce)
            .map(|(p, (i, _))| (p.clone(), *i))
            .collect();
        for (path, i) in due {
            self.pending.remove(&path);
            if i >= self.sources.len() || !self.sources[i].open {
                continue;
            }
            self.upsert_path(i, path, false, None);
        }
    }

    fn upsert_path(
        &mut self,
        i: usize,
        path: PathBuf,
        include_existing: bool,
        min_age_override: Option<Duration>,
    ) {
        let policy = self.policy(i, include_existing, min_age_override);
        let snap = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(cand) = consider_path(
            &path,
            &self.sources[i].cfg,
            &policy,
            &self.ignore,
            &snap,
            SystemTime::now(),
        ) else {
            return;
        };
        self.upsert_candidate(i, path, cand);
    }

    fn upsert_candidate(&mut self, i: usize, path: PathBuf, cand: Candidate) {
        let mut table = self.candidates.lock().unwrap_or_else(|e| e.into_inner());
        let result = upsert(&mut table, path, cand, self.max_pending, SystemTime::now());
        drop(table);
        if matches!(result, Upsert::Rejected | Upsert::ReplacedOldest) {
            self.note_overflow(&self.sources[i].cfg.id);
        }
    }

    #[cfg(windows)]
    fn remove_path(&mut self, path: &Path) {
        self.pending.remove(path);
        self.candidates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(path);
    }

    fn open_source(&mut self, i: usize) {
        if self.sources[i].dead {
            return;
        }
        let Some(resolved) = self.sources[i].resolved.clone() else {
            return;
        };
        #[cfg(windows)]
        if self.sources[i].rdc.is_some() {
            self.begin_close(i, true);
            return;
        }
        match prepare_source(&resolved, &self.folders) {
            Ok(watch_path) => {
                #[cfg(windows)]
                {
                    if !self.attach_rdc(i, &watch_path) {
                        self.schedule_retry(i, false);
                        return;
                    }
                }
                self.sources[i].watch_path = Some(watch_path);
                self.sources[i].open = true;
                self.sources[i].backoff_i = 0;
            }
            Err(OpenError::Missing) => {
                tracing::error!(
                    id = %self.sources[i].cfg.id,
                    path = %resolved.display(),
                    "source directory missing"
                );
                self.schedule_retry(i, true);
            }
            Err(OpenError::Denied(reason)) => {
                tracing::error!(
                    id = %self.sources[i].cfg.id,
                    path = %resolved.display(),
                    ?reason,
                    "source rejected"
                );
                self.sources[i].dead = true;
            }
            Err(OpenError::Failed(err)) => {
                tracing::error!(
                    id = %self.sources[i].cfg.id,
                    path = %resolved.display(),
                    %err,
                    "source open failed"
                );
                self.schedule_retry(i, false);
            }
        }
    }

    fn schedule_retry(&mut self, i: usize, missing: bool) {
        self.sources[i].open = false;
        self.sources[i].watch_path = None;
        let wait = if missing {
            MISSING_RETRY
        } else {
            let idx = self.sources[i].backoff_i.min(OPEN_BACKOFF_SECS.len() - 1);
            self.sources[i].backoff_i = (idx + 1).min(OPEN_BACKOFF_SECS.len() - 1);
            Duration::from_secs(OPEN_BACKOFF_SECS[idx])
        };
        self.sources[i].next_open = Instant::now() + wait;
    }

    fn close_source(&mut self, i: usize) {
        self.sources[i].open = false;
        #[cfg(windows)]
        self.begin_close(i, false);
    }

    fn source_lost(&mut self, i: usize) {
        tracing::error!(
            id = %self.sources[i].cfg.id,
            path = ?self.sources[i].watch_path,
            "source directory lost"
        );
        self.close_source(i);
        self.sources[i].watch_path = None;
        self.sources[i].next_open = Instant::now() + MISSING_RETRY;
    }
}

impl SourceSlot {
    fn from_config(src: &SourceConfig, folders: &KnownFolders) -> Self {
        let now = Instant::now();
        let resolved = src.resolved_path(folders);
        let (dead, resolved) = match resolved {
            None => {
                tracing::error!(
                    id = %src.id,
                    kind = ?src.kind,
                    "known folder unresolved; source disabled"
                );
                (true, None)
            }
            Some(p) => (false, Some(p)),
        };
        Self {
            cfg: src.clone(),
            resolved,
            watch_path: None,
            next_scan: now,
            next_open: now,
            backoff_i: 0,
            open: false,
            dead,
            #[cfg(windows)]
            rdc: None,
            #[cfg(windows)]
            rdc_fails: 0,
            #[cfg(windows)]
            rdc_closing: false,
            #[cfg(windows)]
            reopen_after_close: false,
        }
    }
}

enum OpenError {
    Missing,
    Denied(crate::known_folders::DenyReason),
    Failed(io::Error),
}

fn prepare_source(path: &Path, folders: &KnownFolders) -> Result<PathBuf, OpenError> {
    if let Some(reason) = deny_source(path, folders) {
        return Err(OpenError::Denied(reason));
    }
    if !path.is_dir() {
        return Err(OpenError::Missing);
    }
    #[cfg(windows)]
    {
        resolve_windows_source(path, folders)
    }
    #[cfg(not(windows))]
    {
        normalize_path(path).ok_or_else(|| {
            OpenError::Failed(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source path normalize failed",
            ))
        })
    }
}

fn first_run_from(state: &AppState) -> Option<SystemTime> {
    state
        .first_run_unix()
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

#[cfg(not(windows))]
fn inspect_unix(path: &Path) -> io::Result<FileInfo> {
    let meta = fs::symlink_metadata(path)?;
    let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
    let now = SystemTime::now();
    let created = match meta.created() {
        Ok(c) if c > now => mtime,
        Ok(c) => c,
        Err(_) => mtime,
    };
    Ok(FileInfo {
        size: meta.len(),
        mtime,
        created,
        is_file: meta.file_type().is_file(),
        placeholder: meta.file_type().is_symlink(),
    })
}

#[cfg(any(test, windows))]
fn parse_notify_buffer(buf: &[u8], bytes: usize) -> Vec<(u32, String)> {
    let bytes = bytes.min(buf.len());
    let mut offset = 0usize;
    let mut out = Vec::new();
    while offset.saturating_add(12) <= bytes {
        let next = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap());
        let action = u32::from_le_bytes(buf[offset + 4..offset + 8].try_into().unwrap());
        let name_len =
            u32::from_le_bytes(buf[offset + 8..offset + 12].try_into().unwrap()) as usize;
        let name_start = offset + 12;
        let name_end = name_start.saturating_add(name_len);
        if name_end > bytes {
            break;
        }
        let mut units = Vec::with_capacity(name_len / 2);
        let mut i = name_start;
        while i + 1 < name_end {
            units.push(u16::from_le_bytes([buf[i], buf[i + 1]]));
            i += 2;
        }
        out.push((action, String::from_utf16_lossy(&units)));
        if next == 0 {
            break;
        }
        let Some(next_off) = offset.checked_add(next as usize) else {
            break;
        };
        if next_off <= offset {
            break;
        }
        offset = next_off;
    }
    out
}

#[cfg(windows)]
fn create_iocp() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows::Win32::System::IO::CreateIoCompletionPort;
    // SAFETY: INVALID_HANDLE_VALUE with no existing port creates a new completion port.
    match unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, None, 0, 1) } {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::error!(%e, "CreateIoCompletionPort failed; listing only");
            None
        }
    }
}

#[cfg(windows)]
fn path_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(core::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn normalize_windows(path: &Path) -> Option<PathBuf> {
    let raw = path.to_string_lossy();
    if raw.len() > MAX_PATH_BYTES {
        return None;
    }
    let s = raw.replace('/', "\\");
    let stripped = if let Some(rest) = s.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = s.strip_prefix("\\\\?\\") {
        let b = rest.as_bytes();
        if b.len() >= 2 && b[1] == b':' {
            rest.to_string()
        } else {
            tracing::warn!(path = %path.display(), "normalize failed");
            return None;
        }
    } else {
        s
    };
    Some(PathBuf::from(stripped))
}

#[cfg(windows)]
fn inspect_windows(path: &Path) -> io::Result<FileInfo> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetFileAttributesExW, GetFileAttributesW, GetFileExInfoStandard, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
        FILE_ATTRIBUTE_REPARSE_POINT, INVALID_FILE_ATTRIBUTES, WIN32_FILE_ATTRIBUTE_DATA,
    };

    let wide = path_wide(path);
    // SAFETY: `wide` is NUL-terminated UTF-16 and lives for these calls.
    let attrs = unsafe { GetFileAttributesW(PCWSTR::from_raw(wide.as_ptr())) };
    if attrs == INVALID_FILE_ATTRIBUTES {
        return Err(io::Error::last_os_error());
    }
    let placeholder = attrs
        & (FILE_ATTRIBUTE_REPARSE_POINT.0
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS.0
            | FILE_ATTRIBUTE_RECALL_ON_OPEN.0)
        != 0;
    let is_file = attrs & FILE_ATTRIBUTE_DIRECTORY.0 == 0;
    if placeholder {
        return Ok(FileInfo {
            size: 0,
            mtime: UNIX_EPOCH,
            created: UNIX_EPOCH,
            is_file,
            placeholder: true,
        });
    }
    let mut data = WIN32_FILE_ATTRIBUTE_DATA::default();
    unsafe {
        GetFileAttributesExW(
            PCWSTR::from_raw(wide.as_ptr()),
            GetFileExInfoStandard,
            &mut data as *mut _ as *mut core::ffi::c_void,
        )
    }
    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let size = (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow);
    let mtime = filetime_to_system(data.ftLastWriteTime);
    let mut created = filetime_to_system(data.ftCreationTime);
    let now = SystemTime::now();
    if created > now {
        created = mtime;
    }
    Ok(FileInfo {
        size,
        mtime,
        created,
        is_file,
        placeholder: false,
    })
}

#[cfg(windows)]
fn filetime_to_system(ft: windows::Win32::Foundation::FILETIME) -> SystemTime {
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
    let ticks = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    if ticks <= EPOCH_DIFF {
        return UNIX_EPOCH;
    }
    let t = ticks - EPOCH_DIFF;
    UNIX_EPOCH + Duration::new(t / 10_000_000, ((t % 10_000_000) * 100) as u32)
}

#[cfg(windows)]
fn resolve_windows_source(path: &Path, folders: &KnownFolders) -> Result<PathBuf, OpenError> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        GetFileAttributesW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, INVALID_FILE_ATTRIBUTES,
    };

    let wide = path_wide(path);
    // SAFETY: `wide` is NUL-terminated UTF-16 and lives for the call.
    let attrs = unsafe { GetFileAttributesW(PCWSTR::from_raw(wide.as_ptr())) };
    if attrs == INVALID_FILE_ATTRIBUTES {
        return Err(OpenError::Failed(io::Error::last_os_error()));
    }
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0 {
        return normalize_path(path).ok_or_else(|| {
            OpenError::Failed(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source path normalize failed",
            ))
        });
    }
    let probe = open_dir(
        path,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        false,
    )?;
    unsafe {
        let _ = CloseHandle(probe);
    }
    let follow = open_dir(path, FILE_FLAG_BACKUP_SEMANTICS, false)?;
    let final_path = match final_path_from_handle(follow) {
        Ok(p) => p,
        Err(e) => {
            unsafe {
                let _ = CloseHandle(follow);
            }
            return Err(e);
        }
    };
    unsafe {
        let _ = CloseHandle(follow);
    }
    if let Some(reason) = deny_source(&final_path, folders) {
        return Err(OpenError::Denied(reason));
    }
    Ok(final_path)
}

#[cfg(windows)]
fn open_dir(
    path: &Path,
    flags: windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
    overlapped: bool,
) -> Result<windows::Win32::Foundation::HANDLE, OpenError> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let flags = if overlapped {
        flags | FILE_FLAG_OVERLAPPED
    } else {
        flags
    };
    let wide = path_wide(path);
    // SAFETY: `wide` is NUL-terminated UTF-16 and lives for the call.
    unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            flags,
            None,
        )
    }
    .map_err(|e| OpenError::Failed(io::Error::new(io::ErrorKind::Other, e)))
}

#[cfg(windows)]
fn final_path_from_handle(
    handle: windows::Win32::Foundation::HANDLE,
) -> Result<PathBuf, OpenError> {
    use windows::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, VOLUME_NAME_DOS};

    let mut buf = vec![0u16; 32 * 1024];
    // SAFETY: `handle` is an open directory; `buf` is a writable UTF-16 buffer.
    let n = unsafe { GetFinalPathNameByHandleW(handle, &mut buf, VOLUME_NAME_DOS) };
    if n == 0 || n as usize >= buf.len() {
        return Err(OpenError::Failed(io::Error::last_os_error()));
    }
    buf.truncate(n as usize);
    let s = String::from_utf16_lossy(&buf);
    normalize_path(Path::new(&s)).ok_or_else(|| {
        OpenError::Failed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "final path normalize failed",
        ))
    })
}

#[cfg(windows)]
#[derive(Clone, Copy)]
enum IocpKind {
    Ok,
    Overflow,
    Failed,
    Aborted,
    Lost,
}

#[cfg(windows)]
enum IocpEvent {
    Idle,
    Packet {
        key: usize,
        bytes: u32,
        kind: IocpKind,
    },
}

#[cfg(windows)]
impl WatchRuntime {
    fn attach_rdc(&mut self, i: usize, watch_path: &Path) -> bool {
        use windows::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        use windows::Win32::System::IO::CreateIoCompletionPort;

        if self.sources[i].rdc.is_some() {
            return false;
        }
        // Overlapped RDC with no port can never dequeue cancel packets; listing-only.
        let Some(port) = self.iocp else {
            return true;
        };
        let handle = match open_dir(watch_path, FILE_FLAG_BACKUP_SEMANTICS, true) {
            Ok(h) => h,
            Err(OpenError::Failed(err)) => {
                tracing::error!(
                    id = %self.sources[i].cfg.id,
                    path = %watch_path.display(),
                    %err,
                    "watch CreateFile failed"
                );
                return false;
            }
            Err(_) => return false,
        };
        // SAFETY: `handle` is a newly opened directory; `port` is the T1 completion port.
        // The returned handle aliases `port` and must not be closed.
        if unsafe { CreateIoCompletionPort(handle, Some(port), i + 1, 0) }.is_err() {
            tracing::error!(id = %self.sources[i].cfg.id, "associate IOCP failed");
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
            return false;
        }
        let mut rdc = RdcState {
            handle,
            overlapped: Box::new(windows::Win32::System::IO::OVERLAPPED::default()),
            buffer: vec![0u8; self.buf_len],
            pending: false,
        };
        if !issue_rdc(&mut rdc) {
            return false;
        }
        self.sources[i].rdc = Some(rdc);
        self.sources[i].rdc_fails = 0;
        self.sources[i].rdc_closing = false;
        true
    }

    fn begin_close(&mut self, i: usize, reopen: bool) {
        use windows::Win32::System::IO::CancelIo;

        self.sources[i].open = false;
        if self.sources[i].rdc_closing {
            self.sources[i].reopen_after_close |= reopen;
            return;
        }
        self.sources[i].reopen_after_close = reopen;
        let Some(rdc) = self.sources[i].rdc.as_mut() else {
            if reopen && !self.shutting_down {
                self.open_source(i);
            }
            return;
        };
        if rdc.pending && self.iocp.is_some() {
            unsafe {
                let _ = CancelIo(rdc.handle);
            }
            self.sources[i].rdc_closing = true;
            return;
        }
        rdc.pending = false;
        self.sources[i].rdc = None;
        self.sources[i].rdc_fails = 0;
        if reopen && !self.shutting_down {
            self.open_source(i);
        }
    }

    fn finish_close(&mut self, i: usize) {
        if let Some(rdc) = self.sources[i].rdc.as_mut() {
            rdc.pending = false;
        }
        self.sources[i].rdc_closing = false;
        self.sources[i].rdc_fails = 0;
        self.sources[i].rdc = None;
        let reopen = self.sources[i].reopen_after_close;
        self.sources[i].reopen_after_close = false;
        if reopen && !self.shutting_down {
            self.open_source(i);
        }
    }

    fn rebuild_watch(&mut self, i: usize) {
        tracing::error!(id = %self.sources[i].cfg.id, "watch handle rebuild");
        self.begin_close(i, true);
    }

    fn drain_cancels(&mut self) {
        let Some(port) = self.iocp else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        while self.sources.iter().any(|s| s.rdc_closing) && Instant::now() < deadline {
            match poll_iocp(port, Duration::from_millis(50)) {
                IocpEvent::Idle => {}
                IocpEvent::Packet { key, bytes, kind } => {
                    self.on_rdc_packet(key.wrapping_sub(1), bytes, kind);
                }
            }
        }
    }

    fn wait_iocp(&mut self, timeout: Duration) -> bool {
        let Some(port) = self.iocp else {
            thread::sleep(timeout);
            return false;
        };
        match poll_iocp(port, timeout) {
            IocpEvent::Idle => false,
            IocpEvent::Packet { key, bytes, kind } => {
                self.on_rdc_packet(key.wrapping_sub(1), bytes, kind);
                false
            }
        }
    }

    fn on_rdc_packet(&mut self, i: usize, bytes: u32, kind: IocpKind) {
        if i >= self.sources.len() {
            return;
        }
        if self.sources[i].rdc_closing {
            self.finish_close(i);
            return;
        }
        if self.sources[i].rdc.is_none() {
            return;
        }
        if let Some(rdc) = self.sources[i].rdc.as_mut() {
            rdc.pending = false;
        }
        match kind {
            IocpKind::Aborted | IocpKind::Lost => self.source_lost(i),
            IocpKind::Overflow => {
                self.sources[i].rdc_fails = 0;
                tracing::warn!(id = %self.sources[i].cfg.id, "watch_overflow");
                self.scan_one(i, false, None);
                self.reissue_or_rebuild(i);
            }
            IocpKind::Ok => {
                self.sources[i].rdc_fails = 0;
                let events = {
                    let rdc = self.sources[i].rdc.as_ref().unwrap();
                    parse_notify_buffer(&rdc.buffer, bytes as usize)
                };
                self.apply_notify(i, events);
                self.reissue_or_rebuild(i);
            }
            IocpKind::Failed => {
                self.sources[i].rdc_fails = self.sources[i].rdc_fails.saturating_add(1);
                if self.sources[i].rdc_fails >= 5 {
                    self.rebuild_watch(i);
                } else {
                    self.reissue_or_rebuild(i);
                }
            }
        }
    }

    fn apply_notify(&mut self, i: usize, events: Vec<(u32, String)>) {
        use windows::Win32::Storage::FileSystem::{
            FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED,
            FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
        };

        let Some(dir) = self.sources[i].watch_path.clone() else {
            return;
        };
        let now = Instant::now();
        for (action, name) in events {
            let Some(path) = join_top_level(&dir, &name) else {
                continue;
            };
            if action == FILE_ACTION_REMOVED.0 || action == FILE_ACTION_RENAMED_OLD_NAME.0 {
                self.remove_path(&path);
                continue;
            }
            if action == FILE_ACTION_ADDED.0
                || action == FILE_ACTION_MODIFIED.0
                || action == FILE_ACTION_RENAMED_NEW_NAME.0
            {
                if action == FILE_ACTION_MODIFIED.0 {
                    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    st.skipped.retain(|p| p != &path);
                }
                self.pending.insert(path, (i, now));
            }
        }
    }

    fn reissue_or_rebuild(&mut self, i: usize) {
        if self.sources[i].rdc_closing || self.sources[i].rdc.is_none() {
            return;
        }
        if self.sources[i].rdc.as_mut().is_some_and(issue_rdc) {
            return;
        }
        self.rebuild_watch(i);
    }
}

#[cfg(windows)]
fn poll_iocp(port: windows::Win32::Foundation::HANDLE, timeout: Duration) -> IocpEvent {
    use windows::core::HRESULT;
    use windows::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_INVALID_HANDLE, ERROR_NETNAME_DELETED,
        ERROR_NOTIFY_ENUM_DIR, ERROR_OPERATION_ABORTED, ERROR_PATH_NOT_FOUND, WAIT_TIMEOUT,
    };
    use windows::Win32::System::IO::{GetQueuedCompletionStatus, OVERLAPPED};

    let mut bytes = 0u32;
    let mut key = 0usize;
    let mut overlapped: *mut OVERLAPPED = std::ptr::null_mut();
    let ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
    // SAFETY: `port` is a live completion port; out-pointers are valid stack slots.
    let result =
        unsafe { GetQueuedCompletionStatus(port, &mut bytes, &mut key, &mut overlapped, ms) };
    match result {
        Ok(()) => {
            if key == 0 {
                IocpEvent::Idle
            } else if bytes == 0 {
                IocpEvent::Packet {
                    key,
                    bytes,
                    kind: IocpKind::Overflow,
                }
            } else {
                IocpEvent::Packet {
                    key,
                    bytes,
                    kind: IocpKind::Ok,
                }
            }
        }
        Err(e) => {
            if e.code() == HRESULT::from_win32(WAIT_TIMEOUT.0) {
                return IocpEvent::Idle;
            }
            if overlapped.is_null() || key == 0 {
                return IocpEvent::Idle;
            }
            let kind = if e.code() == HRESULT::from_win32(ERROR_OPERATION_ABORTED.0) {
                IocpKind::Aborted
            } else if e.code() == HRESULT::from_win32(ERROR_NOTIFY_ENUM_DIR.0) {
                IocpKind::Overflow
            } else if [
                ERROR_ACCESS_DENIED.0,
                ERROR_INVALID_HANDLE.0,
                ERROR_FILE_NOT_FOUND.0,
                ERROR_PATH_NOT_FOUND.0,
                ERROR_NETNAME_DELETED.0,
            ]
            .into_iter()
            .any(|c| e.code() == HRESULT::from_win32(c))
            {
                IocpKind::Lost
            } else {
                IocpKind::Failed
            };
            IocpEvent::Packet { key, bytes, kind }
        }
    }
}

#[cfg(windows)]
fn issue_rdc(rdc: &mut RdcState) -> bool {
    use windows::core::HRESULT;
    use windows::Win32::Foundation::ERROR_IO_PENDING;
    use windows::Win32::Storage::FileSystem::{
        ReadDirectoryChangesW, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
        FILE_NOTIFY_CHANGE_SIZE,
    };

    let filter =
        FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE;
    // SAFETY: `rdc.buffer` and `rdc.overlapped` stay pinned in RdcState until the matching
    // completion packet is dequeued.
    let result = unsafe {
        ReadDirectoryChangesW(
            rdc.handle,
            rdc.buffer.as_mut_ptr() as *mut core::ffi::c_void,
            rdc.buffer.len() as u32,
            false,
            filter,
            None,
            Some(&mut *rdc.overlapped),
            None,
        )
    };
    match result {
        Ok(()) => {
            rdc.pending = true;
            true
        }
        Err(e) if e.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {
            rdc.pending = true;
            true
        }
        Err(e) => {
            rdc.pending = false;
            tracing::error!(%e, "ReadDirectoryChangesW failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DOWNLOADS_MIN_AGE_SECS;
    use std::sync::atomic::{AtomicU64, Ordering};

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("weekcase-watch-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn downloads(min_age_secs: u64) -> SourceConfig {
        let mut s = SourceConfig::downloads();
        s.min_age_secs = min_age_secs;
        s.path = None;
        s
    }

    fn policy(min_age_secs: u64, first_run: SystemTime) -> AdmitPolicy {
        AdmitPolicy {
            min_age: Duration::from_secs(min_age_secs),
            include_existing: false,
            first_run_at: Some(first_run),
        }
    }

    fn scan_into(
        dir: &Path,
        min_age: u64,
        first_run: SystemTime,
        ignore: &IgnoreSet,
        state: &AppState,
    ) -> HashMap<PathBuf, Candidate> {
        let src = downloads(min_age);
        let policy = policy(min_age, first_run);
        let mut table = HashMap::new();
        scan_dir(
            dir,
            &src,
            &policy,
            ignore,
            state,
            &mut table,
            256,
            SystemTime::now(),
        );
        table
    }

    #[test]
    fn default_downloads_min_age_is_seven_days() {
        assert_eq!(DOWNLOADS_MIN_AGE_SECS, 604_800);
        let src = SourceConfig::downloads();
        assert_eq!(src.min_age_secs, 604_800);
    }

    #[test]
    fn admit_rejects_young_and_accepts_aged() {
        let path = Path::new("/tmp/a.pdf");
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let info = FileInfo {
            size: 1,
            mtime: now,
            created: now - Duration::from_secs(1),
            is_file: true,
            placeholder: false,
        };
        let src = downloads(10);
        assert!(admit(
            path,
            &info,
            &src,
            &policy(10, UNIX_EPOCH),
            &IgnoreSet::empty(),
            &AppState::default(),
            now
        )
        .is_none());
        assert!(admit(
            path,
            &info,
            &src,
            &policy(0, UNIX_EPOCH),
            &IgnoreSet::empty(),
            &AppState::default(),
            now
        )
        .is_some());
    }

    #[test]
    fn collect_admits_errors_when_dir_missing() {
        let src = downloads(0);
        let err = collect_admits(
            Path::new("/no/such/weekcase-watch-src"),
            &src,
            &policy(0, UNIX_EPOCH),
            &IgnoreSet::empty(),
            &AppState::default(),
            SystemTime::now(),
        );
        assert!(err.is_err());
    }

    #[test]
    fn crdownload_is_not_admitted() {
        let dir = temp_dir();
        fs::write(dir.join("a.crdownload"), b"x").unwrap();
        fs::write(dir.join("a.part"), b"x").unwrap();
        let table = scan_into(
            &dir,
            0,
            UNIX_EPOCH,
            &IgnoreSet::empty(),
            &AppState::default(),
        );
        assert!(table.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn subdirectory_is_not_admitted() {
        let dir = temp_dir();
        let sub = dir.join("foo");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("a.pdf"), b"x").unwrap();
        let table = scan_into(
            &dir,
            0,
            UNIX_EPOCH,
            &IgnoreSet::empty(),
            &AppState::default(),
        );
        assert!(table.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn young_file_stays_off_candidate_table() {
        let dir = temp_dir();
        fs::write(dir.join("a.pdf"), b"x").unwrap();
        let table = scan_into(
            &dir,
            86_400,
            UNIX_EPOCH,
            &IgnoreSet::empty(),
            &AppState::default(),
        );
        assert!(table.is_empty(), "{table:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn aged_file_is_admitted_with_injected_min_age() {
        let dir = temp_dir();
        let path = dir.join("a.pdf");
        fs::write(&path, b"x").unwrap();
        let first = SystemTime::now()
            .checked_sub(Duration::from_secs(60))
            .unwrap();
        let table = scan_into(&dir, 0, first, &IgnoreSet::empty(), &AppState::default());
        assert!(table.contains_key(&path), "{table:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn created_before_first_run_is_not_admitted() {
        let dir = temp_dir();
        fs::write(dir.join("old.pdf"), b"x").unwrap();
        let future = SystemTime::now() + Duration::from_secs(3600);
        let table = scan_into(&dir, 0, future, &IgnoreSet::empty(), &AppState::default());
        assert!(table.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn include_existing_bypasses_first_run() {
        let dir = temp_dir();
        let path = dir.join("old.pdf");
        fs::write(&path, b"x").unwrap();
        let src = downloads(0);
        let policy = AdmitPolicy {
            min_age: Duration::ZERO,
            include_existing: true,
            first_run_at: Some(SystemTime::now() + Duration::from_secs(3600)),
        };
        let mut table = HashMap::new();
        scan_dir(
            &dir,
            &src,
            &policy,
            &IgnoreSet::empty(),
            &AppState::default(),
            &mut table,
            256,
            SystemTime::now(),
        );
        assert!(table.contains_key(&path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignore_names_and_self_exe() {
        let dir = temp_dir();
        let exe = dir.join("weekcase.exe");
        fs::write(dir.join("desktop.ini"), b"x").unwrap();
        fs::write(dir.join("Thumbs.db"), b"x").unwrap();
        fs::write(dir.join("~$book.docx"), b"x").unwrap();
        fs::write(&exe, b"mz").unwrap();
        fs::write(dir.join("keep.pdf"), b"x").unwrap();
        let ignore = IgnoreSet {
            exe: Some(exe.clone()),
            ..IgnoreSet::empty()
        };
        let table = scan_into(&dir, 0, UNIX_EPOCH, &ignore, &AppState::default());
        assert_eq!(table.len(), 1);
        assert!(table.contains_key(&dir.join("keep.pdf")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocked_from_is_not_upserted() {
        let dir = temp_dir();
        let path = dir.join("a.pdf");
        fs::write(&path, b"x").unwrap();
        let mut state = AppState::default();
        state.push_blocked(path.clone(), dir.join("dest.pdf"));
        let table = scan_into(&dir, 0, UNIX_EPOCH, &IgnoreSet::empty(), &state);
        assert!(table.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overflow_during_scan_sets_flag_path() {
        let dir = temp_dir();
        for i in 0..10 {
            fs::write(dir.join(format!("{i}.pdf")), b"x").unwrap();
        }
        let src = downloads(0);
        let policy = policy(0, UNIX_EPOCH);
        let mut table = HashMap::new();
        let overflow = scan_dir(
            &dir,
            &src,
            &policy,
            &IgnoreSet::empty(),
            &AppState::default(),
            &mut table,
            4,
            SystemTime::now(),
        );
        assert!(overflow);
        assert_eq!(table.len(), 4);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn notify_parser_reads_top_level_and_skips_nested() {
        let mut buf = vec![0u8; 64];
        let name: Vec<u16> = "a.pdf".encode_utf16().collect();
        buf[0..4].copy_from_slice(&0u32.to_le_bytes());
        buf[4..8].copy_from_slice(&1u32.to_le_bytes());
        buf[8..12].copy_from_slice(&((name.len() * 2) as u32).to_le_bytes());
        for (i, u) in name.iter().enumerate() {
            buf[12 + i * 2..14 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        let events = parse_notify_buffer(&buf, 12 + name.len() * 2);
        assert_eq!(events, vec![(1, "a.pdf".into())]);
        assert!(join_top_level(Path::new("/d"), "sub\\x.pdf").is_none());
        assert!(join_top_level(Path::new("/d"), "ok.pdf").is_some());
    }

    #[test]
    fn first_run_helper_maps_unix() {
        let mut state = AppState::default();
        assert!(first_run_from(&state).is_none());
        state.stamp_first_run(UNIX_EPOCH + Duration::from_secs(42));
        assert_eq!(
            first_run_from(&state),
            Some(UNIX_EPOCH + Duration::from_secs(42))
        );
    }
}

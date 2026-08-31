#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::io;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{mpsc, Arc, Mutex};
use std::time::SystemTime;

use weekcase::config;
use weekcase::known_folders::KnownFolders;
use weekcase::log_init;
use weekcase::paths::Paths;
#[cfg(not(windows))]
use weekcase::stabilize;
use weekcase::state::AppState;
use weekcase::tray;
use weekcase::undo;
#[cfg(not(windows))]
use weekcase::watch;

const SINGLE_INSTANCE_MUTEX: &str = r"Local\Weekcase.SingleInstance";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            tray::show_error(&err.to_string());
            ExitCode::from(1)
        }
    }
}

fn run() -> io::Result<ExitCode> {
    let paths = Paths::resolve()?;
    let _instance = match SingleInstance::acquire()? {
        Some(held) => held,
        None => return Ok(ExitCode::SUCCESS),
    };
    log_init::init(&paths)?;
    let cfg = config::load_or_default(&paths.config_file())?;
    let mut state = AppState::load(&paths.state_file())?;
    if state.first_run_at.is_none() {
        state.stamp_first_run(SystemTime::now());
        if let Err(err) = state.save(&paths.state_file()) {
            tracing::error!(%err, "persist first_run_at failed");
        }
    }
    if let Err(err) = undo::compact_journal(
        &paths.undo_file(),
        undo::UNDO_CAPACITY,
        undo::UNDO_MAX_BYTES,
    ) {
        tracing::error!(%err, "undo compact failed");
    }
    let paused = Arc::new(AtomicBool::new(cfg.general.paused));
    let shutdown = Arc::new(AtomicBool::new(false));
    let archived_today = Arc::new(AtomicU32::new(0));
    #[cfg(windows)]
    if let Err(err) = tray::apply_autostart(cfg.general.start_with_windows) {
        tracing::error!(%err, "autostart apply failed");
    }
    let cfg = Arc::new(Mutex::new(cfg));
    let state = Arc::new(Mutex::new(state));
    let candidates = Arc::new(Mutex::new(HashMap::new()));
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (exec_tx, exec_rx) = mpsc::channel();
    let folders = Arc::new(Mutex::new(KnownFolders::default()));

    #[cfg(not(windows))]
    let (watch, stab, pending_watch_rx, pending_exec_rx) = {
        let resolved = KnownFolders::resolve();
        tracing::info!(
            config = %paths.config_file().display(),
            state = %paths.state_file().display(),
            undo = %paths.undo_file().display(),
            log = %paths.log_file().display(),
            portable = paths.portable,
            sources = cfg.lock().unwrap_or_else(|e| e.into_inner()).sources.len(),
            first_run_at = ?state.lock().unwrap_or_else(|e| e.into_inner()).first_run_at,
            downloads = ?resolved.downloads,
            screenshots = ?resolved.screenshots,
            "weekcase started"
        );
        *folders.lock().unwrap_or_else(|e| e.into_inner()) = resolved;
        let watch = watch::start_watch(
            Arc::new(cfg.lock().unwrap_or_else(|e| e.into_inner()).clone()),
            Arc::clone(&state),
            Arc::clone(&candidates),
            Arc::clone(&folders),
            cmd_rx,
        );
        let stab = stabilize::start_stabilize(
            Arc::clone(&cfg),
            Arc::clone(&candidates),
            Arc::clone(&paused),
            Arc::clone(&shutdown),
            Arc::clone(&folders),
            paths.undo_file(),
            paths.state_file(),
            Arc::clone(&state),
            Arc::clone(&archived_today),
            exec_rx,
        );
        (Some(watch), Some(stab), None, None)
    };

    #[cfg(windows)]
    let (watch, stab, pending_watch_rx, pending_exec_rx) = {
        tracing::info!(
            config = %paths.config_file().display(),
            state = %paths.state_file().display(),
            undo = %paths.undo_file().display(),
            log = %paths.log_file().display(),
            portable = paths.portable,
            sources = cfg.lock().unwrap_or_else(|e| e.into_inner()).sources.len(),
            first_run_at = ?state.lock().unwrap_or_else(|e| e.into_inner()).first_run_at,
            kf_delay_ms = tray::KF_DELAY_MS,
            "weekcase started"
        );
        (None, None, Some(cmd_rx), Some(exec_rx))
    };

    tray::run(tray::App {
        paths,
        folders,
        cfg,
        state,
        candidates,
        paused,
        shutdown,
        archived_today,
        watch,
        watch_tx: cmd_tx,
        pending_watch_rx,
        stab,
        exec_tx,
        pending_exec_rx,
    })
}

struct SingleInstance {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
}

impl SingleInstance {
    fn acquire() -> io::Result<Option<Self>> {
        Self::acquire_named(SINGLE_INSTANCE_MUTEX)
    }

    fn acquire_named(name: &str) -> io::Result<Option<Self>> {
        #[cfg(windows)]
        {
            acquire_win32(name)
        }
        #[cfg(not(windows))]
        {
            let _ = name;
            Ok(Some(Self {}))
        }
    }
}

#[cfg(windows)]
fn acquire_win32(name: &str) -> io::Result<Option<SingleInstance>> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let wide: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated UTF-16 and lives until CreateMutexW returns.
    let handle = unsafe { CreateMutexW(None, true, PCWSTR::from_raw(wide.as_ptr())) }
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(SingleInstance { handle }))
}

#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        if !self.handle.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
            self.handle = HANDLE::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn second_mutex_acquire_is_none() {
        let name = format!(
            r"Local\Weekcase.Test.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let first = SingleInstance::acquire_named(&name)
            .unwrap()
            .expect("first instance owns the mutex");
        let second = SingleInstance::acquire_named(&name).unwrap();
        assert!(second.is_none());
        drop(first);
        let third = SingleInstance::acquire_named(&name).unwrap();
        assert!(third.is_some());
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_stub_allows_start() {
        assert!(SingleInstance::acquire().unwrap().is_some());
    }
}

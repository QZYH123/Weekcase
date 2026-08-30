#![windows_subsystem = "windows"]

use std::io;
use std::process::ExitCode;

use weekcase::config;
use weekcase::known_folders::KnownFolders;
use weekcase::log_init;
use weekcase::paths::Paths;
use weekcase::state::AppState;

const SINGLE_INSTANCE_MUTEX: &str = r"Local\Weekcase.SingleInstance";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("weekcase: {err}");
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
    let state = AppState::load(&paths.state_file())?;
    let folders = KnownFolders::resolve();
    tracing::info!(
        config = %paths.config_file().display(),
        state = %paths.state_file().display(),
        undo = %paths.undo_file().display(),
        log = %paths.log_file().display(),
        portable = paths.portable,
        sources = cfg.sources.len(),
        first_run_at = ?state.first_run_at,
        downloads = ?folders.downloads,
        screenshots = ?folders.screenshots,
        "weekcase started"
    );
    Ok(ExitCode::SUCCESS)
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

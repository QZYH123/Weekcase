use std::env;
use std::io;
use std::path::{Path, PathBuf};

const APP_DIR: &str = "Weekcase";
const PORTABLE_MARKER: &str = "portable.ini";

/// Config lives under Roaming; state / undo / logs under Local. Portable mode
/// collapses both onto `exe_dir\data\`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub local_dir: PathBuf,
    pub portable: bool,
}

impl Paths {
    pub fn resolve() -> io::Result<Self> {
        Ok(Self::resolve_from(
            current_exe_dir()?,
            roaming_root()?,
            local_root()?,
        ))
    }

    pub fn resolve_from(exe_dir: PathBuf, appdata: PathBuf, local_appdata: PathBuf) -> Self {
        if exe_dir.join(PORTABLE_MARKER).is_file() {
            let data = exe_dir.join("data");
            return Self {
                config_dir: data.clone(),
                local_dir: data,
                portable: true,
            };
        }
        Self {
            config_dir: appdata.join(APP_DIR),
            local_dir: local_appdata.join(APP_DIR),
            portable: false,
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn state_file(&self) -> PathBuf {
        self.local_dir.join("state.json")
    }

    pub fn undo_file(&self) -> PathBuf {
        self.local_dir.join("undo.jsonl")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.local_dir.join("logs")
    }

    pub fn log_file(&self) -> PathBuf {
        self.logs_dir().join("weekcase.log")
    }
}

fn current_exe_dir() -> io::Result<PathBuf> {
    let exe = env::current_exe()?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "executable has no parent directory",
        )
    })
}

fn roaming_root() -> io::Result<PathBuf> {
    if let Some(p) = env_path("APPDATA") {
        return Ok(p);
    }
    #[cfg(windows)]
    {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "APPDATA is not set",
        ))
    }
    #[cfg(not(windows))]
    {
        Ok(home_dir()?.join(".config"))
    }
}

fn local_root() -> io::Result<PathBuf> {
    if let Some(p) = env_path("LOCALAPPDATA") {
        return Ok(p);
    }
    #[cfg(windows)]
    {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is not set",
        ))
    }
    #[cfg(not(windows))]
    {
        Ok(home_dir()?.join(".local").join("share"))
    }
}

fn env_path(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

#[cfg(not(windows))]
fn home_dir() -> io::Result<PathBuf> {
    env_path("HOME").ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let dir = env::temp_dir().join(format!("weekcase-paths-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn roaming_and_local_layout() {
        let paths = Paths::resolve_from(
            PathBuf::from("/exe"),
            PathBuf::from("/roaming"),
            PathBuf::from("/local"),
        );
        assert!(!paths.portable);
        assert_eq!(paths.config_dir, PathBuf::from("/roaming/Weekcase"));
        assert_eq!(paths.local_dir, PathBuf::from("/local/Weekcase"));
        assert_eq!(
            paths.config_file(),
            PathBuf::from("/roaming/Weekcase/config.toml")
        );
        assert_eq!(
            paths.state_file(),
            PathBuf::from("/local/Weekcase/state.json")
        );
        assert_eq!(
            paths.undo_file(),
            PathBuf::from("/local/Weekcase/undo.jsonl")
        );
        assert_eq!(
            paths.log_file(),
            PathBuf::from("/local/Weekcase/logs/weekcase.log")
        );
    }

    #[test]
    fn portable_marker_collapses_onto_data() {
        let root = temp_dir();
        let exe_dir = root.join("exe");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::write(exe_dir.join(PORTABLE_MARKER), "").unwrap();
        let paths = Paths::resolve_from(exe_dir.clone(), root.join("roaming"), root.join("local"));
        assert!(paths.portable);
        assert_eq!(paths.config_dir, exe_dir.join("data"));
        assert_eq!(paths.local_dir, exe_dir.join("data"));
        assert_eq!(
            paths.log_file(),
            exe_dir.join("data").join("logs").join("weekcase.log")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_marker_is_not_portable() {
        let root = temp_dir();
        let exe_dir = root.join("exe");
        fs::create_dir_all(&exe_dir).unwrap();
        let paths = Paths::resolve_from(exe_dir, root.join("roaming"), root.join("local"));
        assert!(!paths.portable);
        assert_eq!(paths.config_dir, root.join("roaming").join("Weekcase"));
        let _ = fs::remove_dir_all(&root);
    }
}

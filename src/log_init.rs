use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;

use crate::paths::Paths;

const MAX_LOG_BYTES: u64 = 1024 * 1024;
const MAX_LOG_FILES: u32 = 3;
const LOG_FILE_NAME: &str = "weekcase.log";

pub fn init(paths: &Paths) -> io::Result<WorkerGuard> {
    let dir = paths.logs_dir();
    fs::create_dir_all(&dir)?;
    let rolling = SizeRollingFile::open(&dir, LOG_FILE_NAME, MAX_LOG_BYTES, MAX_LOG_FILES)?;
    let (writer, guard) = tracing_appender::non_blocking(rolling);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .try_init()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    Ok(guard)
}

/// Current file plus numbered backups (`weekcase.log`, `.1`, `.2` for 3 files).
struct SizeRollingFile {
    dir: PathBuf,
    name: String,
    file: Option<File>,
    written: u64,
    max_bytes: u64,
    files: u32,
}

impl SizeRollingFile {
    fn open(dir: &Path, name: &str, max_bytes: u64, files: u32) -> io::Result<Self> {
        let path = dir.join(name);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            dir: dir.to_path_buf(),
            name: name.to_string(),
            file: Some(file),
            written,
            max_bytes,
            files,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }
        let current = self.dir.join(&self.name);
        if self.files >= 2 {
            let last = self.files - 1;
            let _ = fs::remove_file(self.dir.join(format!("{}.{last}", self.name)));
            for i in (1..last).rev() {
                let from = self.dir.join(format!("{}.{i}", self.name));
                let to = self.dir.join(format!("{}.{}", self.name, i + 1));
                if from.exists() {
                    let _ = fs::rename(&from, &to);
                }
            }
            if current.exists() {
                fs::rename(&current, self.dir.join(format!("{}.1", self.name)))?;
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&current)?;
        self.written = file.metadata()?.len();
        self.file = Some(file);
        Ok(())
    }
}

impl Write for SizeRollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written >= self.max_bytes {
            self.rotate()?;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "log file is not open"))?;
        let n = file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("weekcase-log-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rolls_and_keeps_three_files() {
        let dir = temp_dir();
        let mut w = SizeRollingFile::open(&dir, LOG_FILE_NAME, 32, 3).unwrap();
        for _ in 0..20 {
            w.write_all(&[b'x'; 16]).unwrap();
        }
        w.flush().unwrap();
        drop(w);

        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert!(names.contains(&LOG_FILE_NAME.to_string()), "{names:?}");
        assert!(names.len() <= 3, "{names:?}");
        assert!(names.len() >= 2, "{names:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_oversize_file_rotates_on_next_write() {
        let dir = temp_dir();
        let path = dir.join(LOG_FILE_NAME);
        fs::write(&path, vec![b'a'; 64]).unwrap();
        let mut w = SizeRollingFile::open(&dir, LOG_FILE_NAME, 32, 3).unwrap();
        w.write_all(b"next").unwrap();
        w.flush().unwrap();
        drop(w);
        assert!(dir.join(format!("{LOG_FILE_NAME}.1")).exists());
        let current = fs::read(&path).unwrap();
        assert_eq!(current, b"next");
        let _ = fs::remove_dir_all(&dir);
    }
}

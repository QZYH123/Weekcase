use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const STATE_VERSION: u32 = 1;
pub const MAX_BLOCKED: usize = 64;
pub const MAX_SKIPPED: usize = 256;

/// `state.json`. Intentionally no watermark: young files stay on disk until min_age.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    pub v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_run_at: Option<String>,
    pub blocked: Vec<Blocked>,
    pub skipped: Vec<PathBuf>,
    pub overflow_unacked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blocked {
    pub from: PathBuf,
    pub to: PathBuf,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            v: STATE_VERSION,
            first_run_at: None,
            blocked: Vec::new(),
            skipped: Vec::new(),
            overflow_unacked: false,
        }
    }
}

impl AppState {
    pub fn new_first_run(at: SystemTime) -> Self {
        let mut state = Self::default();
        state.stamp_first_run(at);
        state
    }

    pub fn stamp_first_run(&mut self, at: SystemTime) {
        if self.first_run_at.is_none() {
            self.first_run_at = Some(format_rfc3339_utc(unix_secs(at)));
        }
    }

    pub fn first_run_unix(&self) -> Option<u64> {
        self.first_run_at.as_deref().and_then(parse_rfc3339_utc)
    }

    pub fn is_blocked_from(&self, from: &Path) -> bool {
        self.blocked.iter().any(|b| b.from == from)
    }

    pub fn is_skipped(&self, path: &Path) -> bool {
        self.skipped.iter().any(|p| p == path)
    }

    pub fn push_blocked(&mut self, from: PathBuf, to: PathBuf) {
        self.blocked.retain(|b| b.from != from);
        if self.blocked.len() >= MAX_BLOCKED {
            self.blocked.remove(0);
        }
        self.blocked.push(Blocked { from, to });
    }

    pub fn push_skipped(&mut self, path: PathBuf) {
        self.skipped.retain(|p| p != &path);
        if self.skipped.len() >= MAX_SKIPPED {
            self.skipped.remove(0);
        }
        self.skipped.push(path);
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Self::from_slice(&bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn from_slice(bytes: &[u8]) -> io::Result<Self> {
        let mut state: Self = serde_json::from_slice(bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if state.v == 0 {
            state.v = STATE_VERSION;
        }
        cap_vec(&mut state.blocked, MAX_BLOCKED);
        cap_vec(&mut state.skipped, MAX_SKIPPED);
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(path, bytes)
    }
}

fn cap_vec<T>(items: &mut Vec<T>, max: usize) {
    if items.len() > max {
        let drop = items.len() - max;
        items.drain(..drop);
    }
}

fn unix_secs(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn format_rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hh = rem / 3_600;
    let mm = (rem % 3_600) / 60;
    let ss = rem % 60;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn parse_rfc3339_utc(s: &str) -> Option<u64> {
    if s.len() != 20 || !s.ends_with('Z') {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let y: i32 = s[0..4].parse().ok()?;
    let m: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    let hh: u64 = s[11..13].parse().ok()?;
    let mm: u64 = s[14..16].parse().ok()?;
    let ss: u64 = s[17..19].parse().ok()?;
    if !(1..=12).contains(&m) || d == 0 || d > 31 || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    let days = days_from_civil(y, m, d)?;
    Some(days * 86_400 + hh * 3_600 + mm * 60 + ss)
}

/// Howard Hinnant civil-from-days; `z` is days since 1970-01-01.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + i64::from(m <= 2);
    (y as i32, m, d)
}

fn days_from_civil(y: i32, m: u32, d: u32) -> Option<u64> {
    let mut y = i64::from(y);
    y -= i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + i64::from(doe) - 719_468;
    u64::try_from(days).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn temp_state_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "weekcase-state-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn rfc3339_unix_epoch_and_known_instants() {
        assert_eq!(format_rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(format_rfc3339_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(
            parse_rfc3339_utc("2001-09-09T01:46:40Z"),
            Some(1_000_000_000)
        );
        let stamp = "2026-08-30T00:00:00Z";
        let secs = parse_rfc3339_utc(stamp).unwrap();
        assert_eq!(format_rfc3339_utc(secs), stamp);
    }

    #[test]
    fn load_ignores_watermark_and_does_not_write_it() {
        let json = r#"{
            "v": 1,
            "first_run_at": "2026-08-30T12:00:00Z",
            "blocked": [{"from": "C:\\a", "to": "C:\\b"}],
            "skipped": ["C:\\skip"],
            "overflow_unacked": true,
            "watermark": "do-not-keep"
        }"#;
        let state = AppState::from_slice(json.as_bytes()).unwrap();
        assert_eq!(state.first_run_at.as_deref(), Some("2026-08-30T12:00:00Z"));
        assert_eq!(state.blocked.len(), 1);
        assert!(state.overflow_unacked);
        let value: Value = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(value.get("watermark").is_none());
        assert_eq!(value["v"], 1);
    }

    #[test]
    fn missing_file_is_empty_state() {
        let path = PathBuf::from("/no/such/weekcase/state.json");
        let state = AppState::load(&path).unwrap();
        assert_eq!(state, AppState::default());
        assert!(state.first_run_at.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn first_run_stamp_is_sticky() {
        let mut state = AppState::default();
        let t0 = UNIX_EPOCH + std::time::Duration::from_secs(10);
        let t1 = UNIX_EPOCH + std::time::Duration::from_secs(20);
        state.stamp_first_run(t0);
        state.stamp_first_run(t1);
        assert_eq!(state.first_run_unix(), Some(10));
    }

    #[test]
    fn blocked_and_skipped_are_capped() {
        let mut state = AppState::default();
        for i in 0..70 {
            state.push_blocked(PathBuf::from(format!("/from/{i}")), PathBuf::from("/to"));
        }
        assert_eq!(state.blocked.len(), MAX_BLOCKED);
        assert_eq!(state.blocked[0].from, PathBuf::from("/from/6"));
        for i in 0..300 {
            state.push_skipped(PathBuf::from(format!("/skip/{i}")));
        }
        assert_eq!(state.skipped.len(), MAX_SKIPPED);
        assert_eq!(state.skipped[0], PathBuf::from("/skip/44"));
    }

    #[test]
    fn save_round_trip() {
        let path = temp_state_path();
        let mut state = AppState::new_first_run(UNIX_EPOCH);
        state.push_skipped(PathBuf::from("/tmp/a"));
        state.save(&path).unwrap();
        let loaded = AppState::load(&path).unwrap();
        assert_eq!(loaded, state);
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("watermark"));
        let _ = fs::remove_file(&path);
    }
}

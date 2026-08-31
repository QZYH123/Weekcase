use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::config::SourceKind;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub source_id: String,
    pub source_kind: SourceKind,
    pub first_seen: SystemTime,
    pub last_size: u64,
    pub last_mtime: SystemTime,
    pub created: SystemTime,
    pub stable_since: Option<SystemTime>,
    pub attempts: u32,
    pub poisoned: bool,
    pub settle_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub source_id: String,
    pub source_kind: SourceKind,
    pub size: u64,
    pub mtime: SystemTime,
    pub created: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Inserted,
    Existing,
    ReplacedOldest,
    Rejected,
}

impl Candidate {
    pub fn is_ready(&self, now: SystemTime) -> bool {
        let Some(since) = self.stable_since else {
            return false;
        };
        now.duration_since(since)
            .map(|d| d >= Duration::from_secs(self.settle_secs))
            .unwrap_or(true)
    }

    pub fn occupies_ready_slot(&self, now: SystemTime) -> bool {
        self.poisoned || self.is_ready(now)
    }

    pub fn snapshot(&self, path: PathBuf) -> FileSnapshot {
        FileSnapshot {
            path,
            source_id: self.source_id.clone(),
            source_kind: self.source_kind,
            size: self.last_size,
            mtime: self.last_mtime,
            created: self.created,
        }
    }
}

pub fn upsert(
    table: &mut HashMap<PathBuf, Candidate>,
    path: PathBuf,
    cand: Candidate,
    max: usize,
    now: SystemTime,
) -> Upsert {
    if table.contains_key(&path) {
        return Upsert::Existing;
    }
    if table.len() < max {
        table.insert(path, cand);
        return Upsert::Inserted;
    }
    let oldest = table
        .iter()
        .filter(|(_, c)| !c.occupies_ready_slot(now))
        .min_by_key(|(_, c)| c.first_seen)
        .map(|(p, _)| p.clone());
    match oldest {
        Some(old) => {
            table.remove(&old);
            table.insert(path, cand);
            Upsert::ReplacedOldest
        }
        None => Upsert::Rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn cand(seen: u64, ready: bool) -> Candidate {
        Candidate {
            source_id: "downloads".into(),
            source_kind: SourceKind::Downloads,
            first_seen: UNIX_EPOCH + Duration::from_secs(seen),
            last_size: 1,
            last_mtime: UNIX_EPOCH,
            created: UNIX_EPOCH,
            stable_since: ready.then_some(UNIX_EPOCH),
            attempts: 0,
            poisoned: false,
            settle_secs: 0,
        }
    }

    #[test]
    fn overflow_drops_oldest_not_ready() {
        let mut table = HashMap::new();
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        for i in 0..256 {
            let r = upsert(
                &mut table,
                PathBuf::from(format!("/f/{i}")),
                cand(i as u64, false),
                256,
                now,
            );
            assert_eq!(r, Upsert::Inserted);
        }
        let r = upsert(
            &mut table,
            PathBuf::from("/f/new"),
            cand(300, false),
            256,
            now,
        );
        assert_eq!(r, Upsert::ReplacedOldest);
        assert_eq!(table.len(), 256);
        assert!(!table.contains_key(&PathBuf::from("/f/0")));
        assert!(table.contains_key(&PathBuf::from("/f/new")));
    }

    #[test]
    fn overflow_rejects_when_all_ready() {
        let mut table = HashMap::new();
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        for i in 0..256 {
            upsert(
                &mut table,
                PathBuf::from(format!("/r/{i}")),
                cand(i as u64, true),
                256,
                now,
            );
        }
        let r = upsert(
            &mut table,
            PathBuf::from("/r/new"),
            cand(400, false),
            256,
            now,
        );
        assert_eq!(r, Upsert::Rejected);
        assert_eq!(table.len(), 256);
        assert!(!table.contains_key(&PathBuf::from("/r/new")));
    }

    #[test]
    fn existing_path_is_not_a_new_slot() {
        let mut table = HashMap::new();
        let now = UNIX_EPOCH;
        let path = PathBuf::from("/keep");
        assert_eq!(
            upsert(&mut table, path.clone(), cand(1, false), 2, now),
            Upsert::Inserted
        );
        let mut again = cand(9, false);
        again.attempts = 3;
        assert_eq!(
            upsert(&mut table, path.clone(), again, 2, now),
            Upsert::Existing
        );
        assert_eq!(table.len(), 1);
        assert_eq!(table[&path].attempts, 0);
        assert_eq!(table[&path].first_seen, UNIX_EPOCH + Duration::from_secs(1));
    }
}

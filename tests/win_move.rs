#![cfg(windows)]

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use weekcase::classify::{Bucket, FileSnapshot, Placement};
use weekcase::config::{Collision, Config, SourceKind};
use weekcase::execute::{execute_move, undo_last, ExecError};
use weekcase::state::AppState;
use weekcase::undo::{last_undoable, read_records, JournalOp, UndoError};

static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let n = UNIQUE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "weekcase-winmove-{}-{n}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn snap(path: PathBuf, size: u64) -> FileSnapshot {
    FileSnapshot {
        path,
        source_id: "downloads".into(),
        source_kind: SourceKind::Downloads,
        size,
        mtime: SystemTime::now(),
        created: SystemTime::now(),
    }
}

fn placement(dest_dir: PathBuf, dest_name: &str) -> Placement {
    Placement {
        dest_dir,
        dest_name: OsString::from(dest_name),
        bucket: Bucket::Documents,
    }
}

fn cfg_suffix() -> Config {
    let mut cfg = Config::default();
    cfg.destination.collision = Collision::Suffix;
    cfg
}

#[test]
fn move_creates_dest_dir_and_leaves_source_empty() {
    let dir = temp_dir();
    let from = dir.join("src").join("a.pdf");
    fs::create_dir_all(from.parent().unwrap()).unwrap();
    fs::write(&from, b"hello").unwrap();
    let dest_dir = dir.join("archive").join("Documents");
    let rec = execute_move(
        &cfg_suffix(),
        &snap(from.clone(), 5),
        &placement(dest_dir.clone(), "a.pdf"),
        &dir.join("undo.jsonl"),
        &Mutex::new(AppState::default()),
        &dir.join("state.json"),
    )
    .unwrap();
    assert!(!from.exists());
    let dest = dest_dir.join("a.pdf");
    assert!(dest.exists());
    assert_eq!(fs::read(&dest).unwrap(), b"hello");
    assert_eq!(rec.op, JournalOp::Move);
    assert_eq!(rec.to, dest);
    assert_eq!(rec.size, 5);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collision_suffix_does_not_replace_existing() {
    let dir = temp_dir();
    let from = dir.join("src").join("foo.pdf");
    fs::create_dir_all(from.parent().unwrap()).unwrap();
    fs::write(&from, b"new").unwrap();
    let dest_dir = dir.join("dest");
    fs::create_dir_all(&dest_dir).unwrap();
    let existing = dest_dir.join("foo.pdf");
    fs::write(&existing, b"old").unwrap();

    let rec = execute_move(
        &cfg_suffix(),
        &snap(from.clone(), 3),
        &placement(dest_dir.clone(), "foo.pdf"),
        &dir.join("undo.jsonl"),
        &Mutex::new(AppState::default()),
        &dir.join("state.json"),
    )
    .unwrap();
    assert!(!from.exists());
    assert_eq!(fs::read(&existing).unwrap(), b"old");
    let suffixed = dest_dir.join("foo-1.pdf");
    assert_eq!(fs::read(&suffixed).unwrap(), b"new");
    assert_eq!(rec.to, suffixed);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn skip_leaves_source_and_does_not_write_undo() {
    let dir = temp_dir();
    let from = dir.join("src").join("foo.pdf");
    fs::create_dir_all(from.parent().unwrap()).unwrap();
    fs::write(&from, b"source").unwrap();
    let dest_dir = dir.join("dest");
    fs::create_dir_all(&dest_dir).unwrap();
    let dest = dest_dir.join("foo.pdf");
    fs::write(&dest, b"already").unwrap();

    let mut cfg = Config::default();
    cfg.destination.collision = Collision::Skip;
    let err = execute_move(
        &cfg,
        &snap(from.clone(), 6),
        &placement(dest_dir, "foo.pdf"),
        &dir.join("undo.jsonl"),
        &Mutex::new(AppState::default()),
        &dir.join("state.json"),
    )
    .unwrap_err();
    assert!(matches!(err, ExecError::Skipped));
    assert_eq!(fs::read(&from).unwrap(), b"source");
    assert_eq!(fs::read(&dest).unwrap(), b"already");
    assert!(AppState::load(&dir.join("state.json"))
        .unwrap()
        .is_skipped(&from));
    assert!(read_records(&dir.join("undo.jsonl")).unwrap().is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn undo_last_moves_back_and_is_not_repeatable() {
    let dir = temp_dir();
    let from = dir.join("src").join("a.pdf");
    fs::create_dir_all(from.parent().unwrap()).unwrap();
    fs::write(&from, b"hello").unwrap();
    let dest_dir = dir.join("dest");
    let undo = dir.join("undo.jsonl");
    execute_move(
        &cfg_suffix(),
        &snap(from.clone(), 5),
        &placement(dest_dir.clone(), "a.pdf"),
        &undo,
        &Mutex::new(AppState::default()),
        &dir.join("state.json"),
    )
    .unwrap();
    let dest = dest_dir.join("a.pdf");
    assert!(dest.exists());

    let rec = undo_last(&undo).unwrap();
    assert_eq!(rec.op, JournalOp::Undo);
    assert!(from.exists());
    assert!(!dest.exists());
    assert_eq!(fs::read(&from).unwrap(), b"hello");
    assert!(last_undoable(&read_records(&undo).unwrap()).is_none());

    let err = undo_last(&undo).unwrap_err();
    assert!(matches!(err, UndoError::Empty));
    assert!(from.exists());
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
        &cfg_suffix(),
        &snap(from.clone(), 5),
        &placement(dest_dir.clone(), "a.pdf"),
        &undo,
        &Mutex::new(AppState::default()),
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
fn same_path_is_rejected() {
    let dir = temp_dir();
    let from = dir.join("a.pdf");
    fs::write(&from, b"x").unwrap();
    let err = execute_move(
        &cfg_suffix(),
        &snap(from.clone(), 1),
        &placement(dir.clone(), "a.pdf"),
        &dir.join("undo.jsonl"),
        &Mutex::new(AppState::default()),
        &dir.join("state.json"),
    )
    .unwrap_err();
    assert!(matches!(err, ExecError::SamePath));
    assert!(from.exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn zone_identifier_survives_same_volume_move() {
    let dir = temp_dir();
    let from = dir.join("src").join("z.pdf");
    fs::create_dir_all(from.parent().unwrap()).unwrap();
    fs::write(&from, b"zone").unwrap();
    let ads = format!("{}:Zone.Identifier", from.display());
    fs::write(&ads, "[ZoneTransfer]\nZoneId=3\n").unwrap();
    let dest_dir = dir.join("dest");
    execute_move(
        &cfg_suffix(),
        &snap(from.clone(), 4),
        &placement(dest_dir.clone(), "z.pdf"),
        &dir.join("undo.jsonl"),
        &Mutex::new(AppState::default()),
        &dir.join("state.json"),
    )
    .unwrap();
    let dest = dest_dir.join("z.pdf");
    let dest_ads = format!("{}:Zone.Identifier", dest.display());
    let ads_text = fs::read_to_string(&dest_ads).unwrap();
    assert!(ads_text.contains("ZoneId=3"), "{ads_text}");
    let _ = fs::remove_dir_all(&dir);
}

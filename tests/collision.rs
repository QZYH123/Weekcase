use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use weekcase::classify::{Bucket, FileSnapshot, Placement};
use weekcase::config::{Collision, Config, SourceKind};
use weekcase::execute::{
    execute_move, resolve_dest, suffixed_file_name, ExecError, MAX_COLLISION_SUFFIX,
};
use weekcase::state::AppState;
use weekcase::undo::read_records;

static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let n = UNIQUE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "weekcase-coll-{}-{n}-{}",
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

#[test]
fn suffix_inserts_before_last_extension() {
    assert_eq!(
        suffixed_file_name(OsStr::new("foo.pdf"), 0),
        OsString::from("foo.pdf")
    );
    assert_eq!(
        suffixed_file_name(OsStr::new("foo.pdf"), 1),
        OsString::from("foo-1.pdf")
    );
    assert_eq!(
        suffixed_file_name(OsStr::new("foo.PDF"), 2),
        OsString::from("foo-2.PDF")
    );
    assert_eq!(
        suffixed_file_name(OsStr::new("archive.tar.gz"), 1),
        OsString::from("archive.tar-1.gz")
    );
    assert_eq!(
        suffixed_file_name(OsStr::new("LICENSE"), 1),
        OsString::from("LICENSE-1")
    );
    assert_eq!(
        suffixed_file_name(OsStr::new(".gitignore"), 1),
        OsString::from(".gitignore-1")
    );
}

#[test]
fn suffix_picks_first_free_slot() {
    let dest_dir = Path::new("/archive");
    let exists = |p: &Path| {
        let name = p.file_name().unwrap();
        name == "foo.pdf" || name == "foo-1.pdf"
    };
    let dest = resolve_dest(dest_dir, OsStr::new("foo.pdf"), Collision::Suffix, exists).unwrap();
    assert_eq!(dest.file_name().unwrap(), "foo-2.pdf");
}

#[test]
fn original_name_when_free() {
    let dest = resolve_dest(
        Path::new("/archive"),
        OsStr::new("foo.pdf"),
        Collision::Suffix,
        |_| false,
    )
    .unwrap();
    assert_eq!(dest.file_name().unwrap(), "foo.pdf");
}

#[test]
fn suffix_exhausts_at_99() {
    let err = resolve_dest(
        Path::new("/archive"),
        OsStr::new("foo.pdf"),
        Collision::Suffix,
        |_| true,
    )
    .unwrap_err();
    assert!(matches!(err, ExecError::CollisionLimit));
    assert_eq!(
        suffixed_file_name(OsStr::new("foo.pdf"), MAX_COLLISION_SUFFIX),
        OsString::from("foo-99.pdf")
    );
}

#[test]
fn skip_when_dest_exists() {
    let err = resolve_dest(
        Path::new("/archive"),
        OsStr::new("foo.pdf"),
        Collision::Skip,
        |_| true,
    )
    .unwrap_err();
    assert!(matches!(err, ExecError::Skipped));
}

#[test]
fn skip_leaves_source_and_records_skipped() {
    let dir = temp_dir();
    let src_dir = dir.join("src");
    let dest_dir = dir.join("dest");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&dest_dir).unwrap();
    let from = src_dir.join("foo.pdf");
    let dest = dest_dir.join("foo.pdf");
    fs::write(&from, b"source").unwrap();
    fs::write(&dest, b"already").unwrap();

    let mut cfg = Config::default();
    cfg.destination.collision = Collision::Skip;
    let err = execute_move(
        &cfg,
        &snap(from.clone(), 6),
        &placement(dest_dir.clone(), "foo.pdf"),
        &dir.join("undo.jsonl"),
        &dir.join("state.json"),
    )
    .unwrap_err();
    assert!(matches!(err, ExecError::Skipped));
    assert_eq!(fs::read(&from).unwrap(), b"source");
    assert_eq!(fs::read(&dest).unwrap(), b"already");
    let state = AppState::load(&dir.join("state.json")).unwrap();
    assert!(state.is_skipped(&from));
    assert!(read_records(&dir.join("undo.jsonl")).unwrap().is_empty());
    let _ = fs::remove_dir_all(&dir);
}

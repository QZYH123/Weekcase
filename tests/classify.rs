use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use weekcase::classify::{classify, Bucket, ClassifyError, FileSnapshot};
use weekcase::config::{Config, SourceKind};
use weekcase::known_folders::KnownFolders;

fn folders() -> KnownFolders {
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

fn cfg_with_root(root: &str) -> Config {
    let mut cfg = Config::default();
    cfg.destination.root = Some(root.to_string());
    cfg
}

fn snap(kind: SourceKind, path: &str, created: SystemTime) -> FileSnapshot {
    FileSnapshot {
        path: PathBuf::from(path),
        source_id: match kind {
            SourceKind::Downloads => "downloads".into(),
            SourceKind::Screenshots => "screenshots".into(),
        },
        source_kind: kind,
        size: 1,
        mtime: created,
        created,
    }
}

fn created_utc(date_midnight: u64) -> SystemTime {
    // Noon UTC keeps the civil date under Windows local time west of UTC.
    UNIX_EPOCH + Duration::from_secs(date_midnight + 12 * 3600)
}

fn dest_str(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}

#[test]
fn pdf_goes_to_downloads_documents() {
    let placement = classify(
        &cfg_with_root(r"C:\Users\a\Documents\Weekcase"),
        &snap(
            SourceKind::Downloads,
            r"C:\Users\a\Downloads\a.PDF",
            created_utc(1_788_048_000),
        ),
        &folders(),
    )
    .unwrap();
    assert_eq!(
        dest_str(&placement.dest_dir),
        r"C:\Users\a\Documents\Weekcase\Downloads\Documents"
    );
    assert_eq!(placement.dest_name, OsString::from("a.PDF"));
    assert_eq!(placement.bucket, Bucket::Documents);
}

#[test]
fn screenshot_uses_created_calendar_month() {
    let placement = classify(
        &cfg_with_root(r"C:\Users\a\Documents\Weekcase"),
        &snap(
            SourceKind::Screenshots,
            r"C:\Users\a\Pictures\Screenshots\Screenshot (3).png",
            created_utc(1_788_048_000),
        ),
        &folders(),
    )
    .unwrap();
    assert_eq!(
        dest_str(&placement.dest_dir),
        r"C:\Users\a\Documents\Weekcase\Screenshots\2026-08"
    );
    assert_eq!(placement.dest_name, OsString::from("Screenshot (3).png"));
    assert_eq!(placement.bucket, Bucket::Screenshots);
}

#[test]
fn license_without_extension_is_other() {
    let placement = classify(
        &cfg_with_root(r"C:\Users\a\Documents\Weekcase"),
        &snap(
            SourceKind::Downloads,
            r"C:\Users\a\Downloads\LICENSE",
            created_utc(1_788_048_000),
        ),
        &folders(),
    )
    .unwrap();
    assert_eq!(
        dest_str(&placement.dest_dir),
        r"C:\Users\a\Documents\Weekcase\Downloads\Other"
    );
    assert_eq!(placement.dest_name, OsString::from("LICENSE"));
    assert_eq!(placement.bucket, Bucket::Other);
}

#[test]
fn root_inside_downloads_is_dest_inside_source() {
    let err = classify(
        &cfg_with_root(r"C:\Users\a\Downloads\Weekcase"),
        &snap(
            SourceKind::Downloads,
            r"C:\Users\a\Downloads\a.PDF",
            created_utc(1_788_048_000),
        ),
        &folders(),
    )
    .unwrap_err();
    assert_eq!(err, ClassifyError::DestInsideSource);

    let err = classify(
        &cfg_with_root(r"C:\Users\a\Downloads\Weekcase"),
        &snap(
            SourceKind::Screenshots,
            r"C:\Users\a\Pictures\Screenshots\Screenshot (3).png",
            created_utc(1_788_048_000),
        ),
        &folders(),
    )
    .unwrap_err();
    assert_eq!(err, ClassifyError::DestInsideSource);
}

#[test]
fn unknown_week_token_is_bad_template() {
    let mut cfg = cfg_with_root(r"C:\Users\a\Documents\Weekcase");
    cfg.destination.screenshots_template = "{root}/Screenshots/{ww}".into();
    let err = classify(
        &cfg,
        &snap(
            SourceKind::Screenshots,
            r"C:\Users\a\Pictures\Screenshots\Screenshot (3).png",
            created_utc(1_788_048_000),
        ),
        &folders(),
    )
    .unwrap_err();
    assert_eq!(err, ClassifyError::BadTemplate);
}

#[test]
fn last_dot_only_and_screenshots_ignore_extension() {
    let cfg = cfg_with_root(r"C:\Users\a\Documents\Weekcase");
    let f = folders();
    let gz = classify(
        &cfg,
        &snap(
            SourceKind::Downloads,
            r"C:\Users\a\Downloads\archive.tar.gz",
            created_utc(1_788_048_000),
        ),
        &f,
    )
    .unwrap();
    assert_eq!(gz.bucket, Bucket::Archives);
    assert_eq!(
        dest_str(&gz.dest_dir),
        r"C:\Users\a\Documents\Weekcase\Downloads\Archives"
    );

    let zip = classify(
        &cfg,
        &snap(
            SourceKind::Screenshots,
            r"C:\Users\a\Pictures\Screenshots\foo.zip",
            created_utc(1_788_048_000),
        ),
        &f,
    )
    .unwrap();
    assert_eq!(zip.bucket, Bucket::Screenshots);
    assert_eq!(
        dest_str(&zip.dest_dir),
        r"C:\Users\a\Documents\Weekcase\Screenshots\2026-08"
    );
}

#[test]
fn screenshot_month_follows_created_not_archive_time() {
    let cfg = cfg_with_root(r"C:\Users\a\Documents\Weekcase");
    let f = folders();
    let jan = classify(
        &cfg,
        &snap(
            SourceKind::Screenshots,
            r"C:\Users\a\Pictures\Screenshots\a.png",
            created_utc(1_767_225_600),
        ),
        &f,
    )
    .unwrap();
    assert_eq!(
        dest_str(&jan.dest_dir),
        r"C:\Users\a\Documents\Weekcase\Screenshots\2026-01"
    );

    let nye = classify(
        &cfg,
        &snap(
            SourceKind::Screenshots,
            r"C:\Users\a\Pictures\Screenshots\a.png",
            created_utc(1_767_139_200),
        ),
        &f,
    )
    .unwrap();
    assert_eq!(
        dest_str(&nye.dest_dir),
        r"C:\Users\a\Documents\Weekcase\Screenshots\2025-12"
    );

    let nye26 = classify(
        &cfg,
        &snap(
            SourceKind::Screenshots,
            r"C:\Users\a\Pictures\Screenshots\a.png",
            created_utc(1_798_675_200),
        ),
        &f,
    )
    .unwrap();
    assert_eq!(
        dest_str(&nye26.dest_dir),
        r"C:\Users\a\Documents\Weekcase\Screenshots\2026-12"
    );
}

#[test]
fn illegal_names_and_denylist_dest() {
    let cfg = cfg_with_root(r"C:\Users\a\Documents\Weekcase");
    let f = folders();
    assert_eq!(
        classify(
            &cfg,
            &snap(
                SourceKind::Downloads,
                r"C:\Users\a\Downloads\.",
                created_utc(1_788_048_000),
            ),
            &f,
        )
        .unwrap_err(),
        ClassifyError::IllegalName
    );
    assert_eq!(
        classify(
            &cfg,
            &snap(
                SourceKind::Downloads,
                r"C:\Users\a\Downloads\..",
                created_utc(1_788_048_000),
            ),
            &f,
        )
        .unwrap_err(),
        ClassifyError::IllegalName
    );
    assert_eq!(
        classify(
            &cfg,
            &FileSnapshot {
                path: PathBuf::new(),
                source_id: "downloads".into(),
                source_kind: SourceKind::Downloads,
                size: 1,
                mtime: created_utc(1_788_048_000),
                created: created_utc(1_788_048_000),
            },
            &f,
        )
        .unwrap_err(),
        ClassifyError::IllegalName
    );

    let mut win = cfg_with_root(r"C:\Windows\Weekcase");
    assert_eq!(
        classify(
            &win,
            &snap(
                SourceKind::Downloads,
                r"C:\Users\a\Downloads\a.PDF",
                created_utc(1_788_048_000),
            ),
            &f,
        )
        .unwrap_err(),
        ClassifyError::DestInsideSource
    );

    win.destination.root = Some(r"C:\".into());
    win.destination.downloads_template = "{root}".into();
    assert_eq!(
        classify(
            &win,
            &snap(
                SourceKind::Downloads,
                r"C:\Users\a\Downloads\a.PDF",
                created_utc(1_788_048_000),
            ),
            &f,
        )
        .unwrap_err(),
        ClassifyError::DestInsideSource
    );
}

#[test]
fn relative_or_whole_yyyy_mm_token_is_bad_template() {
    let mut cfg = cfg_with_root(r"C:\Users\a\Documents\Weekcase");
    cfg.destination.downloads_template = "Downloads/{bucket}".into();
    assert_eq!(
        classify(
            &cfg,
            &snap(
                SourceKind::Downloads,
                r"C:\Users\a\Downloads\a.PDF",
                created_utc(1_788_048_000),
            ),
            &folders(),
        )
        .unwrap_err(),
        ClassifyError::BadTemplate
    );

    cfg.destination.downloads_template = "{root}/Downloads/{bucket}".into();
    cfg.destination.screenshots_template = "{root}/Screenshots/{yyyy-mm}".into();
    assert_eq!(
        classify(
            &cfg,
            &snap(
                SourceKind::Screenshots,
                r"C:\Users\a\Pictures\Screenshots\a.png",
                created_utc(1_788_048_000),
            ),
            &folders(),
        )
        .unwrap_err(),
        ClassifyError::BadTemplate
    );
}

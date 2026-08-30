use std::path::{Path, PathBuf};

use weekcase::config::{
    self, unknown_template_tokens, Collision, Config, SourceKind, DOWNLOADS_MIN_AGE_SECS,
    DOWNLOADS_SCAN_INTERVAL_SECS, DOWNLOADS_SETTLE_SECS, MAX_PENDING, MAX_SOURCES,
    RDC_BUFFER_BYTES, SCREENSHOTS_MIN_AGE_SECS, SCREENSHOTS_SCAN_INTERVAL_SECS,
    SCREENSHOTS_SETTLE_SECS, V1_TEMPLATE_TOKENS,
};
use weekcase::known_folders::{
    deny_destination_with, dest_inside_any_source, DenyReason, KnownFolders,
};

#[test]
fn minimal_toml_parses_defaults() {
    let cfg = config::parse(include_str!("../testdata/config_minimal.toml")).unwrap();
    let downloads = cfg
        .sources
        .iter()
        .find(|s| s.kind == SourceKind::Downloads)
        .expect("downloads source");
    let screenshots = cfg
        .sources
        .iter()
        .find(|s| s.kind == SourceKind::Screenshots)
        .expect("screenshots source");

    assert_eq!(downloads.min_age_secs, DOWNLOADS_MIN_AGE_SECS);
    assert_eq!(downloads.min_age_secs, 604_800);
    assert_eq!(downloads.settle_secs, DOWNLOADS_SETTLE_SECS);
    assert_eq!(downloads.scan_interval_secs, DOWNLOADS_SCAN_INTERVAL_SECS);
    assert_eq!(screenshots.min_age_secs, SCREENSHOTS_MIN_AGE_SECS);
    assert_eq!(screenshots.settle_secs, SCREENSHOTS_SETTLE_SECS);
    assert_eq!(
        screenshots.scan_interval_secs,
        SCREENSHOTS_SCAN_INTERVAL_SECS
    );
    assert_eq!(cfg.destination.collision, Collision::Suffix);
    assert!(cfg.general.start_with_windows);
    assert_eq!(cfg.watch.max_pending, MAX_PENDING);
    assert_eq!(cfg.watch.max_sources, MAX_SOURCES);
    assert_eq!(cfg.watch.buffer_bytes, RDC_BUFFER_BYTES);
    assert_eq!(cfg.watch.tick_ms, 1_000);
    assert_eq!(cfg.watch.debounce_ms, 500);
}

#[test]
fn v1_template_token_list() {
    assert_eq!(V1_TEMPLATE_TOKENS, ["root", "bucket", "yyyy", "mm"]);
    assert!(!V1_TEMPLATE_TOKENS.contains(&"ww"));
    let cfg = Config::default();
    assert!(unknown_template_tokens(&cfg.destination.downloads_template).is_empty());
    assert!(unknown_template_tokens(&cfg.destination.screenshots_template).is_empty());
    assert_eq!(
        config::template_tokens("{root}/Screenshots/{yyyy}-{mm}"),
        ["root", "yyyy", "mm"]
    );
    assert_eq!(
        unknown_template_tokens("{root}/Screenshots/{ww}"),
        vec!["ww"]
    );
}

#[test]
fn dest_inside_source_with_injected_paths() {
    let downloads = PathBuf::from(r"C:\Users\a\Downloads");
    let screenshots = PathBuf::from(r"C:\Users\a\Pictures\Screenshots");
    let sources = vec![downloads.clone(), screenshots];
    let folders = KnownFolders {
        profile: Some(PathBuf::from(r"C:\Users\a")),
        documents: Some(PathBuf::from(r"C:\Users\a\Documents")),
        ..KnownFolders::default()
    };

    assert!(dest_inside_any_source(
        Path::new(r"C:\Users\a\Downloads\Weekcase"),
        &sources
    ));
    assert!(dest_inside_any_source(
        Path::new("C:/Users/a/Downloads"),
        &sources
    ));
    assert!(!dest_inside_any_source(
        Path::new(r"C:\Users\a\Documents\Weekcase"),
        &sources
    ));
    assert_eq!(
        deny_destination_with(
            Path::new(r"C:\Users\a\Downloads\out"),
            &sources,
            &folders,
            None
        ),
        Some(DenyReason::DestInsideSource)
    );
    assert_eq!(
        deny_destination_with(
            Path::new(r"C:\Users\a\Documents\Weekcase"),
            &sources,
            &folders,
            None
        ),
        None
    );
}

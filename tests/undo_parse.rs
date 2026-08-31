use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use weekcase::undo::{
    append_record, compact_journal, compact_records, last_undoable, parse_records, read_records,
    JournalOp, JournalRecord, PROTOCOL_VERSION, UNDO_CAPACITY,
};

static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let n = UNIQUE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "weekcase-undo-{}-{n}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn rec(id: &str, op: JournalOp, from: &str, to: &str) -> JournalRecord {
    JournalRecord {
        v: PROTOCOL_VERSION,
        id: id.into(),
        ts: "2026-08-30T12:00:00Z".into(),
        op,
        from: PathBuf::from(from),
        to: PathBuf::from(to),
        size: 4,
        source_id: "downloads".into(),
    }
}

fn jsonl(records: &[JournalRecord]) -> String {
    records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[test]
fn parse_skips_junk_and_unknown_version() {
    let text = r#"
{"v":1,"id":"a","ts":"2026-08-30T12:00:00Z","op":"move","from":"/s/a.pdf","to":"/d/a.pdf","size":3,"source_id":"downloads"}
not json
{"v":2,"id":"x","ts":"2026-08-30T12:00:00Z","op":"move","from":"/s/x.pdf","to":"/d/x.pdf","size":1,"source_id":"downloads"}
{"v":1,"id":"a","ts":"2026-08-30T12:00:01Z","op":"undo","from":"/s/a.pdf","to":"/d/a.pdf","size":3,"source_id":"downloads","extra":true}
{"v":1,"id":"b","ts":"2026-08-30T12:00:02Z","op":"move","from":"/s/b.pdf","to":"/d/b.pdf","size":4,"source_id":"downloads"}
"#;
    let records = parse_records(text);
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].op, JournalOp::Move);
    assert_eq!(records[1].op, JournalOp::Undo);
    assert_eq!(records[1].id, "a");
    assert_eq!(records[2].id, "b");
}

#[test]
fn last_undoable_is_last_move_without_later_undo() {
    let records = parse_records(
        r#"{"v":1,"id":"a","ts":"2026-08-30T12:00:00Z","op":"move","from":"/s/a.pdf","to":"/d/a.pdf","size":1,"source_id":"downloads"}
{"v":1,"id":"b","ts":"2026-08-30T12:00:01Z","op":"move","from":"/s/b.pdf","to":"/d/b.pdf","size":1,"source_id":"downloads"}
{"v":1,"id":"a","ts":"2026-08-30T12:00:02Z","op":"undo","from":"/s/a.pdf","to":"/d/a.pdf","size":1,"source_id":"downloads"}
{"v":1,"id":"c","ts":"2026-08-30T12:00:03Z","op":"move","from":"/s/c.pdf","to":"/d/c.pdf","size":1,"source_id":"downloads"}
"#,
    );
    assert_eq!(last_undoable(&records).unwrap().id, "c");

    let after_c = parse_records(
        &(jsonl(&records)
            + &serde_json::to_string(&rec("c", JournalOp::Undo, "/s/c.pdf", "/d/c.pdf")).unwrap()
            + "\n"),
    );
    assert_eq!(last_undoable(&after_c).unwrap().id, "b");

    let after_b = {
        let mut v = after_c.clone();
        v.push(rec("b", JournalOp::Undo, "/s/b.pdf", "/d/b.pdf"));
        v
    };
    assert!(last_undoable(&after_b).is_none());
}

#[test]
fn second_undo_does_not_select_the_same_move() {
    let records = vec![
        rec("a", JournalOp::Move, "/s/a.pdf", "/d/a.pdf"),
        rec("a", JournalOp::Undo, "/s/a.pdf", "/d/a.pdf"),
    ];
    assert!(last_undoable(&records).is_none());
}

#[test]
fn compact_keeps_undos_for_retained_moves() {
    let records = vec![
        rec("a", JournalOp::Move, "/s/a.pdf", "/d/a.pdf"),
        rec("b", JournalOp::Move, "/s/b.pdf", "/d/b.pdf"),
        rec("b", JournalOp::Undo, "/s/b.pdf", "/d/b.pdf"),
    ];
    let kept = compact_records(&records, 1);
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[0].id, "b");
    assert_eq!(kept[0].op, JournalOp::Move);
    assert_eq!(kept[1].op, JournalOp::Undo);
    assert!(last_undoable(&kept).is_none());

    let undone_old = vec![
        rec("a", JournalOp::Move, "/s/a.pdf", "/d/a.pdf"),
        rec("a", JournalOp::Undo, "/s/a.pdf", "/d/a.pdf"),
        rec("b", JournalOp::Move, "/s/b.pdf", "/d/b.pdf"),
    ];
    let kept = compact_records(&undone_old, 1);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, "b");
    assert_eq!(last_undoable(&kept).unwrap().id, "b");
}

#[test]
fn compact_journal_rewrites_when_over_capacity() {
    let dir = temp_dir();
    let path = dir.join("undo.jsonl");
    for i in 0..5 {
        append_record(
            &path,
            &rec(&format!("id{i}"), JournalOp::Move, "/s/x", "/d/x"),
        )
        .unwrap();
    }
    append_record(&path, &rec("id4", JournalOp::Undo, "/s/x", "/d/x")).unwrap();
    assert!(compact_journal(&path, 2, 1).unwrap());
    let kept = read_records(&path).unwrap();
    let ids: Vec<_> = kept.iter().map(|r| (r.id.as_str(), r.op)).collect();
    assert_eq!(
        ids,
        vec![
            ("id3", JournalOp::Move),
            ("id4", JournalOp::Move),
            ("id4", JournalOp::Undo),
        ]
    );
    assert_eq!(last_undoable(&kept).unwrap().id, "id3");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn compact_skips_when_under_limits() {
    let dir = temp_dir();
    let path = dir.join("undo.jsonl");
    append_record(&path, &rec("a", JournalOp::Move, "/s/a", "/d/a")).unwrap();
    assert!(!compact_journal(&path, UNDO_CAPACITY, 1024 * 1024).unwrap());
    assert_eq!(read_records(&path).unwrap().len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn append_is_one_json_object_per_line() {
    let dir = temp_dir();
    let path = dir.join("undo.jsonl");
    append_record(&path, &rec("a", JournalOp::Move, "/s/a", "/d/a")).unwrap();
    append_record(&path, &rec("a", JournalOp::Undo, "/s/a", "/d/a")).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        assert!(line.starts_with('{'));
        assert!(serde_json::from_str::<JournalRecord>(line).is_ok());
        assert!(!line.contains('\n'));
    }
    assert!(Path::new(&path).exists());
    let _ = fs::remove_dir_all(&dir);
}

#![cfg(feature = "cli")]
use _native::dumps::{
    canonical_json_sha256, stage_supplement, write_canonical_json, SupplementKind,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orphanwiki-supplements-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, value: &Value) -> PathBuf {
        let path = self.0.join("supplement.json");
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn graph() -> Value {
    json!({"language":"vec","database":"vecwiki","source":"Wikimedia SQL dumps / vecwiki",
        "complete":true,"sql_release":"20261001","finished_at":"2026-10-02T12:00:00+00:00",
        "dump_manifest_sha256":"approved-manifest"})
}

fn fixture(kind: SupplementKind, count: usize) -> (Value, &'static str) {
    let g = graph();
    let mut meta =
        json!({"language":"vec","database":"vecwiki","sql_release":"20261001","complete":true});
    let pages = (0..count)
        .rev()
        .map(|i| {
            let mut row = json!({"page_id":i+1,"revision_id":i+100,"title":"È 🌙", "decimal":1e-7});
            match kind {
                SupplementKind::History => {
                    row["article_created_at"] = if i % 3 == 0 {
                        Value::Null
                    } else {
                        json!("2020-01-02T00:00:00Z")
                    };
                    row["creator_history_status"] = json!(if i % 3 == 1 {
                        "observed_surviving_history"
                    } else {
                        "unattributed_history"
                    });
                    row["creator_key"] = json!("[\"vecwiki\", 42]");
                    row["creator_tenure_days"] = json!(0.9859704508633369);
                }
                SupplementKind::Quality => {
                    row["quality_revision_relation"] =
                        json!(["same", "different", "unavailable"][i % 3]);
                    row["quality_status"] = json!(if i % 3 == 2 {
                        "missing_current_page"
                    } else {
                        "scored"
                    });
                    row["quality_score"] = match i % 3 {
                        0 => json!(0.0),
                        1 => json!(0.9859704508633369),
                        _ => Value::Null,
                    };
                    row["content_revision_id"] = match i % 3 {
                        0 => json!(i + 100),
                        1 => json!(i + 200),
                        _ => Value::Null,
                    };
                }
                SupplementKind::Topics => {
                    let labels: Value =
                        serde_json::from_str(include_str!("../config/topics.json")).unwrap();
                    row["topic_status"] =
                        json!(["predicted", "no_model_vocabulary", "no_mapped_outlinks"][i % 3]);
                    row["topic_input_count"] = json!(usize::from(i % 3 == 0));
                    row["topic_labels"] = if i % 6 == 0 {
                        json!([])
                    } else if i % 3 == 0 {
                        json!([labels["labels"][0]])
                    } else {
                        Value::Null
                    };
                }
            }
            row
        })
        .collect::<Vec<_>>();
    let table = match kind {
        SupplementKind::History => {
            meta["source"] = json!("MediaWiki History dumps / vecwiki");
            meta["history_release"] = json!("2026-09");
            "history"
        }
        SupplementKind::Quality => {
            meta["source"] = json!("Johnson V2 / mediawiki_content_current");
            meta["model"] = serde_json::from_str(include_str!("../config/quality.json")).unwrap();
            meta["dump_manifest_sha256"] = g["dump_manifest_sha256"].clone();
            meta["graph_finished_at"] = g["finished_at"].clone();
            meta["input_file_sha256"] = json!("raw-checksum");
            meta["census_page_count"] = json!(count);
            meta["quality_revision_rule"] = json!("selected_official_xml_revision_v1");
            meta["scored_pages"] = json!(pages
                .iter()
                .filter(|p| p["quality_status"] == "scored")
                .count());
            meta["missing_pages"] = json!(count - meta["scored_pages"].as_u64().unwrap() as usize);
            for (name, relation) in [
                ("same_revision_pages", "same"),
                ("different_revision_pages", "different"),
                ("unavailable_revision_pages", "unavailable"),
            ] {
                meta[name] = json!(pages
                    .iter()
                    .filter(|p| p["quality_revision_relation"] == relation)
                    .count());
            }
            meta["normalizers"] = json!([1.0, 1e-7, 1e16, 1.2345, 1.0, 1.0]);
            meta["status"] = json!("original-status-is-excluded-from-quality-hash");
            "quality"
        }
        SupplementKind::Topics => {
            meta["source"] = json!("local_model_on_verified_SQL_dump_outlinks");
            meta["model"] = serde_json::from_str(include_str!("../config/topics.json")).unwrap();
            meta["graph_finished_at"] = g["finished_at"].clone();
            meta["input_file_sha256"] = json!("raw-checksum");
            meta["predicted_pages"] = json!(pages
                .iter()
                .filter(|p| p["topic_status"] == "predicted")
                .count());
            meta["missing_pages"] =
                json!(count - meta["predicted_pages"].as_u64().unwrap() as usize);
            "topics"
        }
    };
    (json!({"metadata":meta,"pages":pages}), table)
}

fn stage_fails(value: &Value, kind: SupplementKind, count: usize) {
    let w = Workspace::new();
    let path = w.write(value);
    let original = fs::read(&path).unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE marker(id INTEGER); BEGIN; INSERT INTO marker VALUES(42)")
        .unwrap();
    assert!(
        stage_supplement(&path, &mut db, kind, &graph(), count, Some("raw-checksum")).is_err(),
        "{kind:?}: {value}"
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT id FROM marker", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        42
    );
    assert!(!db.is_autocommit());
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn complete_supplements_keep_every_record_order_hash_zero_and_missing() {
    for kind in [
        SupplementKind::History,
        SupplementKind::Quality,
        SupplementKind::Topics,
    ] {
        let w = Workspace::new();
        let (mut value, table) = fixture(kind, 1103);
        // An individual oversized record must also cross the 64 KiB reader buffer.
        value["pages"][550]["opaque"] = json!("è 🌙".repeat(30000));
        let path = w.write(&value);
        let original = fs::read(&path).unwrap();
        let mut db = Connection::open(w.0.join("private.sqlite")).unwrap();
        let meta =
            stage_supplement(&path, &mut db, kind, &graph(), 1103, Some("raw-checksum")).unwrap();
        assert_eq!(meta["complete"], true);
        if kind == SupplementKind::Topics {
            assert_eq!(meta["status"], "predicted");
            assert_eq!(meta["input_checksum_verified"], true);
            assert!(meta.get("input_sha256").is_none());
        } else {
            let mut hashed = value.clone();
            if kind == SupplementKind::Quality {
                hashed["metadata"].as_object_mut().unwrap().remove("status");
            }
            assert_eq!(
                meta["input_sha256"],
                canonical_json_sha256(&hashed).unwrap()
            );
            assert_eq!(meta["status"], "collected");
            if kind == SupplementKind::History {
                assert_eq!(meta["matched_pages"], 368);
                assert_eq!(meta["unmatched_pages"], 735);
            }
        }
        let mut query = db
            .prepare(&format!("SELECT id,data FROM {table} ORDER BY id DESC"))
            .unwrap();
        let mut rows = query.query([]).unwrap();
        for expected in value["pages"].as_array().unwrap() {
            let row = rows.next().unwrap().unwrap();
            assert_eq!(
                row.get::<_, i64>(0).unwrap(),
                expected["page_id"].as_i64().unwrap()
            );
            let record: Value = serde_json::from_str(&row.get::<_, String>(1).unwrap()).unwrap();
            assert_eq!(&record, expected);
            let mut observations =
                vec![json!({"page_id":record["page_id"],"revision_id":record["revision_id"]})];
            let records =
                std::collections::HashMap::from([(record["page_id"].as_i64().unwrap(), record)]);
            match kind {
                SupplementKind::History => {
                    _native::join_history_rows(&mut observations, &records, "missing").unwrap();
                }
                SupplementKind::Quality => {
                    _native::join_quality_rows(&mut observations, &records).unwrap()
                }
                SupplementKind::Topics => {
                    let labels = meta["model"]["labels"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap().to_owned())
                        .collect();
                    _native::join_topic_rows(&mut observations, &records, &labels).unwrap();
                }
            }
        }
        assert!(rows.next().unwrap().is_none());
        assert_eq!(fs::read(path).unwrap(), original);
    }
}

#[test]
fn stale_sources_models_censuses_and_normalizers_are_rejected() {
    for kind in [
        SupplementKind::History,
        SupplementKind::Quality,
        SupplementKind::Topics,
    ] {
        let (original, _) = fixture(kind, 8);
        for (key, bad) in [
            ("complete", json!(false)),
            ("language", json!("lmo")),
            ("source", json!("unapproved")),
        ] {
            let mut value = original.clone();
            value["metadata"][key] = bad;
            stage_fails(&value, kind, 8);
        }
        let fields = match kind {
            SupplementKind::History => vec!["database", "sql_release"],
            SupplementKind::Quality => vec![
                "database",
                "sql_release",
                "model",
                "dump_manifest_sha256",
                "graph_finished_at",
                "quality_revision_rule",
                "input_file_sha256",
                "census_page_count",
                "scored_pages",
                "missing_pages",
                "same_revision_pages",
            ],
            SupplementKind::Topics => vec![
                "model",
                "graph_finished_at",
                "input_file_sha256",
                "predicted_pages",
                "missing_pages",
            ],
        };
        for key in fields {
            let mut value = original.clone();
            value["metadata"][key] = Value::Null;
            stage_fails(&value, kind, 8);
        }
        stage_fails(&original, kind, 7);
        stage_fails(&original, kind, 9);
    }
    let (original, _) = fixture(SupplementKind::Quality, 8);
    for bad in [
        json!([]),
        json!([1, 2, 3, 4, 5]),
        json!([1, 2, 3, 4, 5, 0]),
        json!([1, 2, 3, 4, 5, -0.1]),
        json!([1, 2, 3, 4, 5, true]),
        json!([1, 2, 3, 4, 5, null]),
    ] {
        let mut value = original.clone();
        value["metadata"]["normalizers"] = bad;
        stage_fails(&value, SupplementKind::Quality, 8);
    }
    let mut value = original.clone();
    value["metadata"]["same_revision_pages"] = json!(u64::MAX);
    value["metadata"]["different_revision_pages"] = json!(1);
    stage_fails(&value, SupplementKind::Quality, 8);
    let mut value = original;
    value["pages"][0]["quality_revision_relation"] = json!("same"); // changes a declared different relation
    stage_fails(&value, SupplementKind::Quality, 8);
}

#[test]
fn duplicate_ids_and_invalid_topic_availability_cannot_leave_partial_staging() {
    for kind in [
        SupplementKind::History,
        SupplementKind::Quality,
        SupplementKind::Topics,
    ] {
        let (original, _) = fixture(kind, 1103);
        for id in [
            json!(true),
            json!(1.0),
            json!(null),
            json!("1"),
            json!(u64::MAX),
            original["pages"][0]["page_id"].clone(),
        ] {
            let mut value = original.clone();
            value["pages"][1102]["page_id"] = id;
            stage_fails(&value, kind, 1103);
        }
    }
    let (original, _) = fixture(SupplementKind::Topics, 7);
    for (key, bad) in [
        ("topic_labels", json!(["unknown.topic"])),
        ("topic_labels", json!(false)),
        ("topic_input_count", json!(true)),
        ("topic_input_count", json!(-1)),
        ("topic_input_count", json!(0)),
        ("topic_status", json!("unknown")),
    ] {
        let mut value = original.clone();
        value["pages"][0][key] = bad;
        stage_fails(&value, SupplementKind::Topics, 7);
    }
    let mut value = original.clone();
    value["pages"][1]["topic_labels"] = json!([]);
    stage_fails(&value, SupplementKind::Topics, 7);
    let mut value = original;
    let label = value["metadata"]["model"]["labels"][0].clone();
    value["pages"][0]["topic_labels"] = json!([label, label]);
    stage_fails(&value, SupplementKind::Topics, 7);
}

#[test]
fn malformed_streams_roll_back_after_large_prefixes_and_preserve_existing_tables() {
    let w = Workspace::new();
    let (value, _) = fixture(SupplementKind::History, 1103);
    let encoded = serde_json::to_string(&value).unwrap();
    let mut bad = vec![
        encoded[..encoded.len() - 2].to_owned(),
        encoded.clone() + " false",
    ];
    let mut extra = value.clone();
    extra["extra"] = json!([]);
    bad.push(serde_json::to_string(&extra).unwrap());
    bad.push(format!(
        "{{\"pages\":[],\"metadata\":{}}}",
        value["metadata"]
    ));
    bad.push(format!(
        "{{\"metadata\":{},\"pages\":[],\"pages\":[]}}",
        value["metadata"]
    ));
    for malformed in [json!(null), json!(true), json!([])] {
        let mut v = value.clone();
        v["pages"][1102] = malformed;
        bad.push(serde_json::to_string(&v).unwrap());
    }
    for text in bad {
        let path = w.0.join("broken.json");
        fs::write(&path, text).unwrap();
        let mut db = Connection::open_in_memory().unwrap();
        assert!(stage_supplement(
            &path,
            &mut db,
            SupplementKind::History,
            &graph(),
            1103,
            None
        )
        .is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM sqlite_master", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(db.is_autocommit());
    }
    let path = w.write(&value);
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE history(id INTEGER PRIMARY KEY,data TEXT); INSERT INTO history VALUES(42,'approved')").unwrap();
    assert!(stage_supplement(
        &path,
        &mut db,
        SupplementKind::History,
        &graph(),
        1103,
        None
    )
    .is_err());
    assert_eq!(
        db.query_row("SELECT data FROM history WHERE id=42", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "approved"
    );
}

#[test]
fn empty_censuses_optional_quality_checksum_and_input_overlap_follow_contracts() {
    let w = Workspace::new();
    for kind in [
        SupplementKind::History,
        SupplementKind::Quality,
        SupplementKind::Topics,
    ] {
        let (value, _) = fixture(kind, 0);
        let path = w.write(&value);
        let mut db = Connection::open_in_memory().unwrap();
        let checksum = if kind == SupplementKind::Topics {
            Some("raw-checksum")
        } else {
            None
        };
        stage_supplement(&path, &mut db, kind, &graph(), 0, checksum).unwrap();
    }
    let (value, _) = fixture(SupplementKind::Topics, 1);
    let path = w.write(&value);
    let mut db = Connection::open_in_memory().unwrap();
    assert!(stage_supplement(&path, &mut db, SupplementKind::Topics, &graph(), 1, None).is_err());
    let path = w.0.join("source.sqlite");
    let mut db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE marker(id INTEGER)").unwrap();
    let before = fs::read(&path).unwrap();
    assert!(stage_supplement(&path, &mut db, SupplementKind::History, &graph(), 0, None).is_err());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn canonical_numbers_strings_and_order_match_the_frozen_python_identity() {
    let tie = "1255183068570128.25".parse::<f64>().unwrap();
    let value = json!({"z":[0,-0.0,1.0,1e-4,1e-5,1e15,1e16,1e20,1e-100,5e-324,tie,0.9859704508633369],"È":"🌙\n\u{7f}\u{1b}","a":true});
    let expected = r#"{"a": true, "z": [0, -0.0, 1.0, 0.0001, 1e-05, 1000000000000000.0, 1e+16, 1e+20, 1e-100, 5e-324, 1255183068570128.2, 0.9859704508633369], "\u00c8": "\ud83c\udf19\n\u007f\u001b"}"#;
    let mut encoded = Vec::new();
    write_canonical_json(&value, &mut encoded).unwrap();
    assert_eq!(String::from_utf8(encoded).unwrap(), expected);
    assert_eq!(
        canonical_json_sha256(&value).unwrap(),
        format!("{:x}", Sha256::digest(expected))
    );
}

#[test]
fn canonical_extremes_and_notation_boundaries_do_not_round_or_change_types() {
    let original = "[1.7976931348623157e+308, 2.2250738585072014e-308, 9.999999999999999e-05, 9999999999999998.0, 1.0000000000000002e+16, -9223372036854775808, 18446744073709551615]";
    let value: serde_json::Value = serde_json::from_str(original).unwrap();
    let mut encoded = Vec::new();
    write_canonical_json(&value, &mut encoded).unwrap();
    assert_eq!(String::from_utf8(encoded).unwrap(), original);
}

#[test]
fn canonical_writer_streams_large_arrays_and_preserves_sink_errors() {
    use std::io::{self, Write};
    struct Sink {
        calls: usize,
        bytes: usize,
        fail_at: usize,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.bytes >= self.fail_at {
                return Err(io::Error::other("synthetic sink failure"));
            }
            assert!(bytes.len() <= 65536);
            self.bytes += bytes.len();
            self.calls += 1;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let value = json!({"metadata":{"complete":true},"pages":(0..10000).map(|i|json!({"page_id":i,"title":"È 🌙","score":1e-7})).collect::<Vec<_>>()});
    let mut sink = Sink {
        calls: 0,
        bytes: 0,
        fail_at: usize::MAX,
    };
    {
        let mut buffered = io::BufWriter::with_capacity(65536, &mut sink);
        write_canonical_json(&value, &mut buffered).unwrap();
        buffered.flush().unwrap();
    }
    assert!(sink.calls > 8);
    assert!(sink.bytes > 500000);
    let mut broken = Sink {
        calls: 0,
        bytes: 0,
        fail_at: 16,
    };
    assert!(write_canonical_json(&value, &mut broken).is_err());
}

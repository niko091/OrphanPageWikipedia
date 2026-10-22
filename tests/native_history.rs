#![cfg(feature = "cli")]

use orphanwiki::dumps::{import_history, HistoryOptions};
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::Digest;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Workspace(PathBuf);
impl Workspace {
    fn new(pages: i64) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orphanwiki-history-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let workspace = Self(path);
        let db = workspace.db();
        db.execute_batch("CREATE TABLE dump_page(id INTEGER,ns INTEGER,redirect INTEGER);
            CREATE TABLE imported(table_name TEXT PRIMARY KEY,sha256 TEXT);
            CREATE TABLE dump_group(user_id INTEGER,name TEXT,expiry TEXT);
            INSERT INTO imported VALUES('user_groups','snapshot');
            INSERT INTO dump_group VALUES(42,'bot','20250101000000'),(43,'bot',NULL),(44,'bot','invalid');").unwrap();
        let mut query = db.prepare("INSERT INTO dump_page VALUES(?,0,0)").unwrap();
        for page in 1..=pages {
            query.execute([page]).unwrap();
        }
        db.execute_batch("INSERT INTO dump_page VALUES(9000000,1,0),(9000001,0,1)")
            .unwrap();
        workspace
    }
    fn db(&self) -> Connection {
        Connection::open(self.0.join("vecwiki-20261001.sqlite")).unwrap()
    }
    fn partition(&self, name: &str, rows: &[Vec<String>]) -> Value {
        let text = rows.iter().map(|r| r.join("\t") + "\n").collect::<String>();
        self.archive(name, text.as_bytes())
    }
    fn archive(&self, name: &str, text: &[u8]) -> Value {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        encoder.write_all(text).unwrap();
        let bytes = encoder.finish().unwrap();
        let path = self.0.join(name);
        fs::write(&path, &bytes).unwrap();
        json!({"path":path,"bytes":bytes.len(),"sha256_local":format!("{:x}",sha2::Sha256::digest(&bytes))})
    }
    fn manifest(&self, files: Value) -> PathBuf {
        let path = self.0.join("manifest.json");
        fs::write(&path,json!({"language":"vec","database":"vecwiki","sql_release":"20261001","history_release":"2026-09","files":files}).to_string()).unwrap();
        path
    }
    fn import(&self, manifest: &Path, options: HistoryOptions) -> Result<Value, String> {
        let output = self.0.join("history.json");
        import_history(manifest, &output, &self.0, options).map_err(|e| e.to_string())?;
        Ok(serde_json::from_slice(&fs::read(output).unwrap()).unwrap())
    }
    fn table(&self, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
        let db = self.db();
        let mut query = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let count = query.column_count();
        query
            .query_map([], |r| (0..count).map(|i| r.get(i)).collect())
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn revision(page: i64, user: i64, at: &str) -> Vec<String> {
    let mut row = vec![String::new(); 78];
    for (index, value) in [
        (0, "vecwiki"),
        (2, "revision"),
        (3, "create"),
        (4, at),
        (31, "0"),
        (37, at),
        (38, at),
        (61, "0"),
    ] {
        row[index] = value.into();
    }
    row[6] = user.to_string();
    row[28] = page.to_string();
    row[60] = (page + 100).to_string();
    row
}

#[test]
fn reconstruction_attribution_current_flags_and_strict_activity_boundaries() {
    let workspace = Workspace::new(15);
    let at = "2026-03-01 12:00:00.0";
    let mut rows = Vec::new();
    for page in 1..=14 {
        let mut row = revision(
            page,
            if page == 11 {
                43
            } else if page >= 12 {
                44
            } else {
                42
            },
            at,
        );
        row[23] = "2025-03-01 12:00:00.0".into();
        match page {
            1 => {
                row[6] = "٤٢".into();
                row[28] = "١".into();
                row[60] = "١٠١".into();
                row[12] = "sysop, bot".into();
                row[77] = "tag-a,tag-b".into();
            }
            2 => row[63] = "text, user_is_deleted ".into(),
            3 => row[61] = "1".into(),
            4 => {
                let mut conflict = row.clone();
                conflict[61] = "1".into();
                conflict[37] = "invalid".into();
                rows.push(conflict);
            }
            5 => row[37] = String::new(),
            6 => row[38] = "2020-01-01 00:00:00".into(),
            7 => row[76] = "true".into(),
            8 => rows.push(row.clone()),
            9 | 10 => {
                let mut event = row.clone();
                event[2] = "page".into();
                event[3] = if page == 9 { "import" } else { "create" }.into();
                event[4] = "2020-01-01 00:00:00".into();
                rows.push(event);
            }
            13 => {
                let mut conflict = row.clone();
                conflict[61] = "1".into();
                conflict[37] = "bad".into();
                rows.push(conflict);
            }
            14 => {
                row[4] = "bad".into();
                row[37] = "bad".into();
                row[38] = "bad".into();
            }
            _ => {}
        }
        rows.push(row);
    }
    // Five January and February edits; namespace fallback and negative namespace.
    for month in [1, 2] {
        for day in 1..=5 {
            let mut row = revision(999, 42, &format!("2026-{month:02}-{day:02} 00:00:00"));
            row[61] = "1".into();
            if month == 2 {
                row[31] = String::new();
                row[33] = "-1".into();
            }
            rows.push(row);
        }
    }
    let mut invalid = revision(999, 44, "invalid");
    invalid[61] = "1".into();
    rows.push(invalid);
    let mut nondecimal = revision(999, 42, "invalid");
    nondecimal[60] = "²".into();
    rows.push(nondecimal);
    let file = workspace.partition("all.tsv.bz2", &rows);
    let manifest = workspace.manifest(json!({"all.tsv.bz2":file}));
    let result = workspace
        .import(&manifest, HistoryOptions::default())
        .unwrap();
    let pages = result["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 15);
    assert_eq!(pages[0]["creator_prior_edits_main_history"], 5);
    assert_eq!(pages[0]["creator_prior_edits_other_history"], 5);
    assert_eq!(pages[0]["creator_continuity_months_before_creation"], 2);
    assert_eq!(pages[0]["creator_tenure_days"], 365.0);
    assert_eq!(pages[0]["creator_prior_articles"], 0);
    assert_eq!(
        pages[0]["creator_groups_at_creation"],
        json!(["sysop", " bot"])
    );
    assert_eq!(pages[0]["creation_tags_history"], json!(["tag-a", "tag-b"]));
    assert_eq!(pages[0]["creator_account_type"], "no_bot_flag");
    assert_eq!(pages[10]["creator_account_type"], "bot");
    assert!(pages[11]["creator_account_type"].is_null());
    assert_eq!(
        pages[11]["creator_history_status"],
        "invalid_history_timestamp"
    );
    assert_eq!(
        pages[12]["creator_history_status"],
        "observed_surviving_history"
    );
    assert!(pages[12]["creator_prior_edits_main_history"].is_null());
    for (page, status) in [
        (2, "history_hidden_or_unregistered_creator"),
        (3, "history_missing_initial_revision"),
        (6, "history_inconsistent_creation_dates"),
        (7, "history_imported_or_ambiguous"),
        (8, "history_ambiguous_initial_revisions"),
        (9, "history_imported_or_ambiguous"),
        (10, "history_create_event_mismatch"),
        (14, "history_inconsistent_creation_dates"),
        (15, "history_missing_initial_revision"),
    ] {
        assert_eq!(
            pages[page - 1]["creator_attribution"],
            status,
            "page {page}"
        );
    }
    assert!(pages[2]["article_created_at"].is_null());
    assert_eq!(
        pages[2].get("creator_prior_edits_main_history"),
        Some(&Value::Null)
    );
    assert!(pages[2].get("creation_tags_history").is_none());

    assert_eq!(pages[2]["creation_date_status"], "reconstructed_history");
    assert_eq!(
        pages[3]["creation_date_status"],
        "ambiguous_history_creation"
    );
    assert_eq!(pages[4]["creation_date_status"], "missing_history_creation");
    assert!(pages[4]["creator_user_id"].is_number());
    assert_eq!(
        result["metadata"]["creation_rule"],
        "mediawiki_history_page_creation_v2"
    );
    let user_hash = format!("{:x}", sha2::Sha256::digest(b"[42, 43, 44]"));
    assert!(workspace
        .table("history_imported")
        .iter()
        .any(|row| matches!(&row[2],rusqlite::types::Value::Text(s) if s.ends_with(&user_hash))));
    // Checkpoint reuse must not execute any data INSERT, even on another backend.
    workspace.db().execute_batch("CREATE TRIGGER reject_initial BEFORE INSERT ON history_initial BEGIN SELECT RAISE(ABORT,'unexpected scan'); END;
        CREATE TRIGGER reject_activity BEFORE INSERT ON history_edit BEGIN SELECT RAISE(ABORT,'unexpected scan'); END;").unwrap();
    assert_eq!(
        workspace
            .import(&manifest, HistoryOptions::default())
            .unwrap(),
        result
    );
}

#[test]
fn bounded_creation_cache_and_sqlite_distinct_keep_identical_records_and_checkpoints() {
    let mut snapshots = Vec::new();
    for creation_cache in [2, 100_000] {
        let workspace = Workspace::new(6001);
        let mut rows = Vec::new();
        for cycle in 0..3 {
            for page in 1..=6001 {
                let mut row = revision(page, 42, "2020-01-01 00:00:00.0");
                row[60] = (page + cycle * 6001).to_string();
                row[61] = if cycle == 0 { "0" } else { "1" }.into();
                rows.push(row);
            }
        }
        let mut conflict = revision(1, 42, "2020-01-01 00:00:00.0");
        conflict[61] = "1".into();
        conflict[37] = "invalid".into();
        rows.push(conflict);
        let file = workspace.partition("all.tsv.bz2", &rows);
        let manifest = workspace.manifest(json!({"all.tsv.bz2":file}));
        let result = workspace
            .import(
                &manifest,
                HistoryOptions {
                    creation_cache,
                    commit_rows: if creation_cache == 2 { 1 } else { 250000 },
                    ..HistoryOptions::default()
                },
            )
            .unwrap();
        let mut tables = [
            "history_initial",
            "history_page_event",
            "history_creation",
            "history_edit",
            "history_imported",
        ]
        .map(|table| workspace.table(table));
        for rows in &mut tables {
            rows.sort_by_key(|row| format!("{row:?}"));
        }
        snapshots.push((result, tables));
    }
    // Only the source path differs between independent synthetic workspaces.
    assert_eq!(snapshots[0], snapshots[1]);
}

#[test]
fn interrupted_partition_preserves_completed_partitions_and_retries_without_duplicates() {
    let workspace = Workspace::new(1);
    let good = workspace.partition("a.tsv.bz2", &[revision(1, 42, "2020-01-01 00:00:00")]);
    let later = revision(1, 42, "2020-01-01 00:00:00");
    let text = (later.join("\t") + "\n").repeat(5001) + "bad\n";
    let broken = workspace.archive("b.tsv.bz2", text.as_bytes());
    let manifest = workspace.manifest(json!({"a.tsv.bz2":good,"b.tsv.bz2":broken}));
    fs::write(
        workspace.0.join("history.json"),
        b"previous approved export",
    )
    .unwrap();
    let options = HistoryOptions {
        commit_rows: 1,
        ..HistoryOptions::default()
    };
    assert!(workspace
        .import(&manifest, options)
        .unwrap_err()
        .contains("5002"));
    assert_eq!(
        fs::read(workspace.0.join("history.json")).unwrap(),
        b"previous approved export"
    );
    assert_eq!(workspace.table("history_initial").len(), 5001);
    assert_eq!(workspace.table("history_imported").len(), 1);
    let fixed = workspace.partition("b.tsv.bz2", &[later]);
    let manifest = workspace.manifest(json!({"a.tsv.bz2":good,"b.tsv.bz2":fixed}));
    let result = workspace.import(&manifest, options).unwrap();
    assert_eq!(workspace.table("history_initial").len(), 2);
    assert_eq!(workspace.table("history_imported").len(), 4);
    assert_eq!(
        result["pages"][0]["creator_attribution"],
        "history_ambiguous_initial_revisions"
    );
    assert_eq!(workspace.import(&manifest, options).unwrap(), result);
}

#[test]
fn modified_inputs_and_output_collisions_never_replace_approved_files() {
    let workspace = Workspace::new(1);
    let file = workspace.partition("all.tsv.bz2", &[revision(1, 42, "2020-01-01 00:00:00")]);
    let manifest = workspace.manifest(json!({"all.tsv.bz2":file}));
    workspace
        .import(&manifest, HistoryOptions::default())
        .unwrap();
    let output = workspace.0.join("history.json");
    let approved = fs::read(&output).unwrap();
    for path in [
        &manifest,
        &workspace.0.join("vecwiki-20261001.sqlite"),
        &workspace.0.join("all.tsv.bz2"),
    ] {
        let bytes = fs::read(path).unwrap();
        assert!(import_history(&manifest, path, &workspace.0, HistoryOptions::default()).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    fs::write(workspace.0.join("all.tsv.bz2"), b"corrupt").unwrap();
    assert!(workspace
        .import(&manifest, HistoryOptions::default())
        .is_err());
    assert_eq!(fs::read(output).unwrap(), approved);
}

#[test]
fn rejected_partition_checkpoint_does_not_approve_committed_partial_rows() {
    let workspace = Workspace::new(1);
    let rows = vec![revision(1, 42, "2020-01-01 00:00:00"); 5001];
    let file = workspace.partition("all.tsv.bz2", &rows);
    let manifest = workspace.manifest(json!({"all.tsv.bz2":file}));
    // Install a failure at checkpoint publication after the first batch commit.
    workspace
        .import(&manifest, HistoryOptions::default())
        .unwrap();
    let approved = fs::read(workspace.0.join("history.json")).unwrap();
    workspace
        .db()
        .execute_batch(
            "DELETE FROM history_imported WHERE phase='initial';
        CREATE TRIGGER reject_checkpoint BEFORE INSERT ON history_imported WHEN NEW.phase='initial'
        BEGIN SELECT RAISE(ABORT,'checkpoint write failed'); END;",
        )
        .unwrap();
    let options = HistoryOptions {
        commit_rows: 1,
        ..HistoryOptions::default()
    };
    assert!(workspace
        .import(&manifest, options)
        .unwrap_err()
        .contains("checkpoint write failed"));
    assert_eq!(workspace.table("history_initial").len(), 5001);
    assert_eq!(workspace.table("history_imported").len(), 1);
    assert_eq!(
        fs::read(workspace.0.join("history.json")).unwrap(),
        approved
    );
    workspace
        .db()
        .execute_batch("DROP TRIGGER reject_checkpoint")
        .unwrap();
    workspace.import(&manifest, options).unwrap();
    assert_eq!(workspace.table("history_initial").len(), 5001);
    assert_eq!(workspace.table("history_imported").len(), 2);
    assert_eq!(
        fs::read(workspace.0.join("history.json")).unwrap(),
        approved
    );
}

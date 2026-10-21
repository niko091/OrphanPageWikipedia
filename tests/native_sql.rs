#![cfg(feature = "cli")]

use _native::dumps::{import_graph, SQL_TABLES};
use serde_json::{json, Value};
use sha2::Digest;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orphanwiki-sql-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn archive(&self, table: &str, columns: &[&str], rows: &[String], trailer: &str) -> Value {
        let mut sql = format!("CREATE TABLE `{table}` (\n");
        for column in columns {
            sql.push_str(&format!("  `{column}` TEXT,\n"));
        }
        sql.push_str(");\n");
        for batch in rows.chunks(64) {
            sql.push_str(&format!(
                "INSERT INTO `{table}` VALUES {};\n",
                batch.join(",")
            ));
        }
        sql.push_str(trailer);
        let path = self.0.join(format!("{table}.sql.gz"));
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(sql.as_bytes()).unwrap();
        let bytes = encoder.finish().unwrap();
        fs::write(&path, &bytes).unwrap();
        json!({"path":path,"bytes":bytes.len(),"sha256_local":format!("{:x}",sha2::Sha256::digest(&bytes)),"job_updated":"2026-10-02 01:02:03.000001234"})
    }
    fn manifest(&self, modern_categories: bool) -> PathBuf {
        let mut files = serde_json::Map::new();
        let mut pages = (1..=1101)
            .map(|id| format!("({id},0,'Vòce_{id}',0,{},1000)", id * 2))
            .collect::<Vec<_>>();
        pages.extend([
            "(2000,0,'Alias',1,1,1001)".into(),
            "(2001,14,'Specific',0,1,1002)".into(),
            "(2002,14,'Broad',0,1,1003)".into(),
            "(2003,14,'Hidden',0,1,1004)".into(),
        ]);
        files.insert(
            "page".into(),
            self.archive(
                "page",
                &[
                    "page_id",
                    "page_namespace",
                    "page_title",
                    "page_is_redirect",
                    "page_len",
                    "page_latest",
                ],
                &pages,
                "",
            ),
        );
        files.insert(
            "linktarget".into(),
            self.archive(
                "linktarget",
                &["lt_id", "lt_namespace", "lt_title"],
                &[
                    "(1,0,'Vòce_2')".into(),
                    "(2,0,'Alias')".into(),
                    "(3,14,'Specific')".into(),
                    "(4,14,'Broad')".into(),
                    "(5,14,'Hidden')".into(),
                ],
                "",
            ),
        );
        files.insert(
            "pagelinks".into(),
            self.archive(
                "pagelinks",
                &["pl_from", "pl_from_namespace", "pl_target_id"],
                &[
                    "(1,0,2)".into(),
                    "(1,0,1)".into(),
                    "(1,0,1)".into(),
                    "(2,0,1)".into(),
                    "(2000,0,1)".into(),
                    "(2001,14,1)".into(),
                ],
                "",
            ),
        );
        files.insert(
            "redirect".into(),
            self.archive(
                "redirect",
                &["rd_from", "rd_namespace", "rd_title", "rd_interwiki"],
                &["(2000,0,'Vòce_2','')".into()],
                "",
            ),
        );
        files.insert(
            "page_props".into(),
            self.archive(
                "page_props",
                &["pp_page", "pp_propname", "pp_value"],
                &[
                    "(1,'wikibase_item','Q1')".into(),
                    "(2,'wikibase_item','Q2')".into(),
                    "(3,'wikibase_item','Q2')".into(),
                    "(4,'wikibase_item','Q0')".into(),
                    "(2003,'hiddencat','')".into(),
                    "(1,'ignored','value')".into(),
                ],
                "",
            ),
        );
        let (columns, rows) = if modern_categories {
            (
                vec!["cl_from", "cl_target_id"],
                vec![
                    "(1,3)".into(),
                    "(1,4)".into(),
                    "(1,5)".into(),
                    "(2001,4)".into(),
                    "(2002,3)".into(),
                ],
            )
        } else {
            (
                vec!["cl_from", "cl_to"],
                vec![
                    "(1,'Specific')".into(),
                    "(1,'Broad')".into(),
                    "(1,'Hidden')".into(),
                    "(2001,'Broad')".into(),
                    "(2002,'Specific')".into(),
                ],
            )
        };
        files.insert(
            "categorylinks".into(),
            self.archive("categorylinks", &columns, &rows, ""),
        );
        files.insert(
            "user_groups".into(),
            self.archive(
                "user_groups",
                &["ug_user", "ug_group", "ug_expiry"],
                &["(42,'bot',NULL)".into()],
                "",
            ),
        );
        let path = self.0.join("manifest.json");
        fs::write(&path,json!({"language":"vec","database":"vecwiki","sql_release":"20261001","history_release":"2026-09","files":files}).to_string()).unwrap();
        path
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn complete_census_streaming_and_both_category_schemas_preserve_records() {
    let mut graphs = Vec::new();
    for modern in [false, true] {
        let workspace = Workspace::new();
        let manifest = workspace.manifest(modern);
        let output = workspace.0.join("raw.json");
        let work = workspace.0.join("stage");
        assert_eq!(import_graph(&manifest, &output, &work).unwrap(), 1101);
        let raw: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        assert_eq!(raw["pages"].as_array().unwrap().len(), 1101);
        assert_eq!(
            raw["pages"][0]["links"],
            json!(["Alias", "Vòce 2", "Vòce 2"])
        );
        assert_eq!(raw["pages"][0]["categories"], json!(["Specific", "Broad"]));
        assert_eq!(raw["pages"][1100]["links"], json!([]));
        assert_eq!(raw["pages"][1100]["categories"], json!([]));
        assert_eq!(raw["pages"][3]["wikibase_item"], Value::Null);
        assert_eq!(raw["pages"][0]["page_created_at"], Value::Null);
        assert_eq!(raw["redirects"], json!({"Alias":"Vòce 2"}));
        assert_eq!(
            raw["category_parents"],
            json!({"Specific":["Broad"],"Broad":["Specific"]})
        );
        assert_eq!(raw["metadata"]["started_at"], "2026-10-01T00:00:00+00:00");
        assert_eq!(
            raw["metadata"]["finished_at"],
            "2026-10-02T01:02:03.000001+00:00"
        );
        let db = rusqlite::Connection::open(work.join("vecwiki-20261001.sqlite")).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM imported", [], |row| row
                .get::<_, usize>(0))
                .unwrap(),
            SQL_TABLES.len()
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM dump_prop", [], |row| row
                .get::<_, usize>(0))
                .unwrap(),
            5
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM dump_link", [], |row| row
                .get::<_, usize>(0))
                .unwrap(),
            5
        );
        let canonical =
            [&raw["pages"], &raw["redirects"], &raw["category_parents"]].map(Clone::clone);
        assert_eq!(import_graph(&manifest, &output, &work).unwrap(), 1101);
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&output).unwrap()).unwrap(),
            raw
        );
        graphs.push(canonical);
    }
    assert_eq!(graphs[0], graphs[1]);
}

#[test]
fn incomplete_table_retries_without_duplicating_completed_records() {
    let workspace = Workspace::new();
    let path = workspace.manifest(true);
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut changed = original.clone();
    let bad = workspace.archive(
        "pagelinks",
        &["pl_from", "pl_from_namespace", "pl_target_id"],
        &["(1,0,1)".into()],
        "INSERT INTO `pagelinks` VALUES (2,0,1)\n",
    );
    changed["files"]["pagelinks"] = bad;
    fs::write(&path, changed.to_string()).unwrap();
    let output = workspace.0.join("raw.json");
    fs::write(&output, b"previous approved output").unwrap();
    let work = workspace.0.join("stage");
    assert!(import_graph(&path, &output, &work)
        .unwrap_err()
        .to_string()
        .contains("troncato"));
    assert_eq!(fs::read(&output).unwrap(), b"previous approved output");
    let db = rusqlite::Connection::open(work.join("vecwiki-20261001.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM imported", [], |row| row
            .get::<_, usize>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM dump_page", [], |row| row
            .get::<_, usize>(0))
            .unwrap(),
        1105
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM dump_link", [], |row| row
            .get::<_, usize>(0))
            .unwrap(),
        0
    );
    workspace.manifest(true);
    assert_eq!(import_graph(&path, &output, &work).unwrap(), 1101);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM dump_link", [], |row| row
            .get::<_, usize>(0))
            .unwrap(),
        5
    );
}

#[test]
fn modified_archives_and_output_overlap_never_overwrite_inputs() {
    let workspace = Workspace::new();
    let manifest = workspace.manifest(true);
    let before = fs::read(&manifest).unwrap();
    let work = workspace.0.join("stage");
    assert!(import_graph(&manifest, &manifest, &work).is_err());
    assert!(import_graph(&manifest, &work.join("../manifest.json"), &work).is_err());
    assert!(import_graph(&manifest, &work.join("vecwiki-20261001.sqlite"), &work).is_err());
    let input = workspace.0.join("page.sql.gz");
    assert!(import_graph(&manifest, &input, &work).is_err());
    assert!(!work.exists());
    assert_eq!(fs::read(&manifest).unwrap(), before);
    fs::write(&input, b"modified source").unwrap();
    let output = workspace.0.join("raw.json");
    assert!(import_graph(&manifest, &output, &work)
        .unwrap_err()
        .to_string()
        .contains("modificato"));
    assert!(!output.exists());
}

#[test]
fn truncated_gzip_and_empty_censuses_leave_the_previous_export_usable() {
    for empty in [false, true] {
        let workspace = Workspace::new();
        let path = workspace.manifest(true);
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if empty {
            manifest["files"]["page"] = workspace.archive(
                "page",
                &[
                    "page_id",
                    "page_namespace",
                    "page_title",
                    "page_is_redirect",
                    "page_len",
                    "page_latest",
                ],
                &["(1,1,'Talk',0,2,3)".into()],
                "",
            );
        } else {
            let archive = workspace.0.join("page.sql.gz");
            let mut bytes = fs::read(&archive).unwrap();
            bytes.truncate(bytes.len() - 6);
            fs::write(&archive, &bytes).unwrap();
            manifest["files"]["page"]["bytes"] = json!(bytes.len());
            manifest["files"]["page"]["sha256_local"] =
                json!(format!("{:x}", sha2::Sha256::digest(&bytes)));
        }
        fs::write(&path, manifest.to_string()).unwrap();
        let output = workspace.0.join("raw.json");
        fs::write(&output, b"previous approved output").unwrap();
        let work = workspace.0.join("stage");
        assert!(import_graph(&path, &output, &work).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"previous approved output");
        let db = rusqlite::Connection::open(work.join("vecwiki-20261001.sqlite")).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM imported", [], |row| row
                .get::<_, usize>(0))
                .unwrap(),
            if empty { 7 } else { 0 }
        );
    }
}

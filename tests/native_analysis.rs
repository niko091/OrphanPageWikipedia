#![cfg(feature = "cli")]
use _native::dumps::{
    analyze_imported_graph, analyze_memory_graph, canonical_json_sha256, comparison_schema,
    stage_comparison_graph, AnalysisOptions, SQL_TABLES,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Workspace(PathBuf);
impl Workspace {
    fn new(count: usize) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orphanwiki-analysis-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let w = Self(path);
        let manifest = json!({"language":"vec","database":"vecwiki","sql_release":"20261001","files":SQL_TABLES.iter().map(|&t|(t.to_owned(),json!({"sha256_local":t,"path":w.0.join(format!("{t}.sql"))}))).collect::<serde_json::Map<_,_>>()});
        w.write("manifest.json", &manifest);
        let meta = json!({"language":"vec","database":"vecwiki","source":"Wikimedia SQL dumps / vecwiki","complete":true,"sql_release":"20261001","finished_at":"2026-10-02T00:00:00+00:00","dump_manifest":w.0.join("manifest.json"),"dump_manifest_sha256":hash(&fs::read(w.0.join("manifest.json")).unwrap()),"article_features":{"pageviews":"unavailable_not_in_sql_dumps"}});
        let db = w.db();
        db.execute_batch("CREATE TABLE imported(table_name TEXT PRIMARY KEY,sha256 TEXT);CREATE TABLE dump_page(id INTEGER PRIMARY KEY,ns INTEGER,redirect INTEGER,title TEXT);CREATE TABLE dump_redirect(page_id INTEGER,ns INTEGER,title TEXT,interwiki TEXT);CREATE TABLE dump_category(source_id INTEGER,target_id INTEGER,title TEXT);CREATE TABLE dump_target(id INTEGER,ns INTEGER,title TEXT);CREATE TABLE dump_prop(page_id INTEGER,name TEXT);CREATE TABLE pages(id INTEGER PRIMARY KEY,data TEXT);CREATE TABLE links(id INTEGER,target TEXT);CREATE INDEX link_page ON links(id);CREATE TABLE categories(id INTEGER,target TEXT);CREATE INDEX category_page ON categories(id);").unwrap();
        for t in SQL_TABLES {
            db.execute("INSERT INTO imported VALUES(?,?)", [t, t])
                .unwrap();
        }
        let mut pages = Vec::new();
        let mut history = Vec::new();
        let mut quality = Vec::new();
        let mut topics = Vec::new();
        let topic_spec: Value =
            serde_json::from_str(include_str!("../config/topics.json")).unwrap();
        for i in 0..count {
            let id = i * 3 + 10;
            let title = format!("È 🌙 {i}");
            let mut page = json!({"page_id":id,"title":title,"revision_id":id+1000,"wikibase_item":format!("Q{}",i+1),"created_at":null,"page_created_at":null,"creation_date_status":"not_collected","length_bytes":if i==0 {json!(0)} else {Value::Null}});
            db.execute("INSERT INTO dump_page VALUES(?,0,0,?)", params![id, title])
                .unwrap();
            db.execute(
                "INSERT INTO pages VALUES(?,?)",
                params![id, page.to_string()],
            )
            .unwrap();
            let mut links = if i == 0 {
                vec![
                    title.clone(),
                    "Alias".into(),
                    "È 🌙 1".into(),
                    "È 🌙 1".into(),
                    "Missing".into(),
                    "Cycle".into(),
                ]
            } else if i % 4 == 0 && i + 1 < count {
                vec!["È 🌙 1".into()]
            } else {
                vec![]
            };
            if i == 0 {
                links.extend((0..6001).map(|n| format!("Unavailable {n}")));
            }
            let categories = if i == 0 {
                vec!["General", "Specific", "Specific"]
            } else {
                vec!["General", "Maintenance"]
            };
            for target in &links {
                db.execute("INSERT INTO links VALUES(?,?)", params![id, target])
                    .unwrap();
            }
            for label in &categories {
                db.execute("INSERT INTO categories VALUES(?,?)", params![id, label])
                    .unwrap();
            }
            page["links"] = json!(links);
            page["categories"] = json!(categories);
            pages.push(page);
            history.push(json!({"page_id":id,"article_created_at":if i%3==0 {json!("2000-01-01T00:00:00Z")} else {Value::Null},"page_created_at":if i==1 {json!("2040-01-01T00:00:00Z")} else if i%3==0 {Value::Null} else {json!("2020-01-01T00:00:00Z")},"creator_history_status":"observed_surviving_history","creator_key":"vec:42","creator_attribution":"historical_first_revision","creator_tenure_days":0.9859704508633369}));
            quality.push(json!({"page_id":id,"revision_id":id+1000,"content_revision_id":if i%3==2 {Value::Null} else {json!(id+1000+i%3)},"quality_revision_relation":(["same","different","unavailable"][i%3]),"quality_status":if i%3==2 {"missing_current_page"} else {"scored"},"quality_score":match i%3 {0=>json!(0.0),1=>json!(0.9859704508633369),_=>Value::Null}}));
            topics.push(json!({"page_id":id,"revision_id":id+1000,"topic_status":if i%3==1 {"no_mapped_outlinks"} else {"predicted"},"topic_input_count":usize::from(i%3!=1),"topic_labels":if i%3==1 {Value::Null} else if i%3==0 {json!([])} else {json!([topic_spec["labels"][0]])}}));
        }
        for (id, title, target) in [
            (90000, "Alias", "È 🌙 1"),
            (90001, "Cycle", "Cycle2"),
            (90002, "Cycle2", "Cycle"),
        ] {
            db.execute("INSERT INTO dump_page VALUES(?,0,1,?)", params![id, title])
                .unwrap();
            db.execute(
                "INSERT INTO dump_redirect VALUES(?,0,?,'')",
                params![id, target],
            )
            .unwrap();
        }
        db.execute_batch("INSERT INTO dump_page VALUES(90003,14,0,'Specific'),(90004,14,0,'Hidden');INSERT INTO dump_category VALUES(90003,NULL,'General'),(90003,NULL,'Hidden');INSERT INTO dump_prop VALUES(90004,'hiddencat')").unwrap();
        fs::write(w.0.join("raw.json"),format!("{{\"metadata\":{meta},\"pages\":{},\"redirects\":{{\"Alias\":\"È 🌙 1\",\"Cycle\":\"Cycle2\",\"Cycle2\":\"Cycle\"}},\"category_parents\":{{\"Specific\":[\"General\"]}}}}",json!(pages))).unwrap();
        let raw_hash = hash(&fs::read(w.0.join("raw.json")).unwrap());
        w.write("history.json",&json!({"metadata":{"language":"vec","database":"vecwiki","sql_release":"20261001","complete":true,"source":"MediaWiki History dumps / vecwiki","creation_rule":"mediawiki_history_page_creation_v2","history_release":"2026-09"},"pages":history}));
        let quality_spec: Value =
            serde_json::from_str(include_str!("../config/quality.json")).unwrap();
        let mut quality_meta = json!({"language":"vec","database":"vecwiki","sql_release":"20261001","complete":true,"source":"Johnson V2 / mediawiki_content_current","model":quality_spec,"dump_manifest_sha256":meta["dump_manifest_sha256"],"graph_finished_at":meta["finished_at"],"input_file_sha256":raw_hash,"census_page_count":count,"quality_revision_rule":"selected_official_xml_revision_v1","scored_pages":quality.iter().filter(|r|r["quality_status"]=="scored").count(),"missing_pages":quality.iter().filter(|r|r["quality_status"]!="scored").count(),"normalizers":[1,2,3,4,5,6]});
        for relation in ["same", "different", "unavailable"] {
            quality_meta[format!("{relation}_revision_pages")] = json!(quality
                .iter()
                .filter(|r| r["quality_revision_relation"] == relation)
                .count());
        }
        w.write(
            "quality.json",
            &json!({"metadata":quality_meta,"pages":quality}),
        );
        w.write("topics.json",&json!({"metadata":{"language":"vec","source":"local_model_on_verified_SQL_dump_outlinks","complete":true,"model":topic_spec,"graph_finished_at":meta["finished_at"],"input_file_sha256":raw_hash,"predicted_pages":topics.iter().filter(|r|r["topic_status"]=="predicted").count(),"missing_pages":topics.iter().filter(|r|r["topic_status"]!="predicted").count()},"pages":topics}));
        w
    }
    fn db(&self) -> Connection {
        Connection::open(self.0.join("vecwiki-20261001.sqlite")).unwrap()
    }
    fn write(&self, name: &str, value: &Value) {
        fs::write(self.0.join(name), value.to_string()).unwrap();
    }
    fn read(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.0.join(name)).unwrap()).unwrap()
    }
    fn run(
        &self,
        batch_size: usize,
        supplements: bool,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let history = self.0.join("history.json");
        let quality = self.0.join("quality.json");
        let topics = self.0.join("topics.json");
        let rules = json!({"min_support":1,"exclude_categories":["Maintenance"]});
        analyze_imported_graph(
            &self.0.join("raw.json"),
            &self.0.join("result.json"),
            &AnalysisOptions {
                rules: &rules,
                work_dir: Some(&self.0),
                history: supplements.then_some(history.as_path()),
                quality: supplements.then_some(quality.as_path()),
                topics: supplements.then_some(topics.as_path()),
                batch_size,
            },
        )
    }
    fn assert_preserved(&self) {
        fs::write(self.0.join("result.json"), b"approved").unwrap();
        assert!(self.run(7, true).is_err());
        assert_eq!(fs::read(self.0.join("result.json")).unwrap(), b"approved");
        assert!(fs::read_dir(&self.0).unwrap().all(|p| !p
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".orphanwiki-")));
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn bounded_sql_analysis_keeps_the_full_census_all_fanout_and_exact_measures() {
    let w = Workspace::new(523);
    let source_files = [
        "raw.json",
        "manifest.json",
        "vecwiki-20261001.sqlite",
        "history.json",
        "quality.json",
        "topics.json",
    ];
    let before = source_files.map(|p| hash(&fs::read(w.0.join(p)).unwrap()));
    let mut reference = None;
    for batch in [1, 7, 512] {
        let meta = w.run(batch, true).unwrap();
        let result = w.read("result.json");
        assert_eq!(meta, result["metadata"]);
        assert_eq!(meta["unresolved_target_occurrences"], 6003);
        assert_eq!(meta["age_rule"], "mediawiki_history_page_creation_v2");
        assert_eq!(
            meta["article_features"]["pageviews"],
            "unavailable_not_in_sql_dumps"
        );
        let rows = result["pages"].as_array().unwrap();
        assert_eq!(rows.len(), 523);
        assert_eq!(rows[0]["page_id"], 10);
        assert_eq!(rows[0]["in_degree"], 0);
        assert_eq!(rows[0]["out_degree"], 1);
        assert_eq!(rows[1]["in_degree"], 131);
        assert_eq!(rows[522]["in_degree"], 0);
        assert_eq!(rows[522]["out_degree"], 0);
        assert_eq!(rows[0]["category"], "Specific");
        assert_eq!(rows[0]["category_candidates"].as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["quality_score"], 0.0);
        assert_eq!(
            rows[1]["quality_score"].as_f64().unwrap().to_bits(),
            0.9859704508633369f64.to_bits()
        );
        assert_eq!(rows[1]["quality_revision_relation"], "different");
        assert!(rows[2]["quality_score"].is_null());
        assert_eq!(rows[0]["topic_labels"], json!([]));
        assert!(rows[1]["topic_labels"].is_null());
        assert!(rows[0]["age_days"].is_null());
        assert!(rows[0]["history_first_revision_age_days"].as_f64().unwrap() > 9000.0);
        assert_eq!(rows[1]["creation_date_status"], "after_graph_collection");
        assert!(rows[1]["age_days"].is_null());
        assert!(rows[0]["creator_key"].is_null());
        assert_eq!(rows[0]["history_creator_key"], "vec:42");
        let bytes = fs::read(w.0.join("result.json")).unwrap();
        if let Some(expected) = &reference {
            assert_eq!(&bytes, expected);
        } else {
            reference = Some(bytes);
        }
    }
    assert_eq!(
        source_files.map(|p| hash(&fs::read(w.0.join(p)).unwrap())),
        before
    );
    let meta = w.run(512, false).unwrap();
    assert_eq!(meta["history"]["status"], "not_collected");
    assert!(meta.get("quality").is_none());
    assert!(meta.get("topics").is_none());
    assert!(w.read("result.json")["pages"][0]["history_first_revision_age_days"].is_null());
}

#[test]
fn late_invalid_joins_and_foreign_census_ids_cannot_replace_an_approved_member() {
    for (name, field, bad) in [
        ("quality.json", "revision_id", json!(999999)),
        ("quality.json", "quality_score", json!(2.0)),
        ("quality.json", "page_id", json!(999999)),
        ("topics.json", "revision_id", json!(999999)),
        ("history.json", "page_id", json!(999999)),
    ] {
        let w = Workspace::new(523);
        let mut value = w.read(name);
        value["pages"][522][field] = bad;
        w.write(name, &value);
        w.assert_preserved();
    }
}

#[test]
fn invalid_staging_coverage_relations_and_source_identities_are_rejected() {
    for sql in [
        "DELETE FROM imported WHERE table_name='page'",
        "DELETE FROM pages WHERE id=10",
        "UPDATE pages SET data='{}' WHERE id=10",
        "INSERT INTO links VALUES(1,'unknown source')",
        "INSERT INTO categories VALUES(999999,'unknown source')",
        "UPDATE pages SET data=replace(data,'È 🌙 0','changed title') WHERE id=10",
    ] {
        let w = Workspace::new(9);
        w.db().execute_batch(sql).unwrap();
        w.assert_preserved();
    }
    let w = Workspace::new(0);
    w.assert_preserved();
    let w = Workspace::new(9);
    let mut manifest = w.read("manifest.json");
    manifest["sql_release"] = json!("20260901");
    w.write("manifest.json", &manifest);
    w.assert_preserved();
}

#[test]
fn inputs_cannot_be_output_and_invalid_batch_sizes_do_not_write() {
    let w = Workspace::new(9);
    let raw = w.0.join("raw.json");
    let h = w.0.join("history.json");
    for destination in [
        "raw.json",
        "manifest.json",
        "vecwiki-20261001.sqlite",
        "history.json",
        "page.sql",
    ] {
        let output = w.0.join(destination);
        let before = fs::read(&output).ok();
        assert!(analyze_imported_graph(
            &raw,
            &output,
            &AnalysisOptions {
                rules: &json!({}),
                work_dir: Some(&w.0),
                history: Some(&h),
                quality: None,
                topics: None,
                batch_size: 7
            }
        )
        .is_err());
        assert_eq!(fs::read(output).ok(), before);
    }
    assert!(w.run(0, true).is_err());
    assert!(w.run(513, true).is_err());
    assert!(!w.0.join("result.json").exists());
}

#[test]
fn publication_failure_cleans_private_records_and_preserves_existing_paths() {
    let w = Workspace::new(9);
    fs::create_dir(w.0.join("result.json")).unwrap();
    fs::write(w.0.join("result.json/approved"), b"keep").unwrap();
    assert!(w.run(7, true).is_err());
    assert_eq!(fs::read(w.0.join("result.json/approved")).unwrap(), b"keep");
    assert!(fs::read_dir(&w.0).unwrap().all(|p| !p
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".orphanwiki-")));
}

#[test]
fn memory_and_bounded_analysis_preserve_the_same_complete_scientific_rows() {
    let w = Workspace::new(523);
    let raw = w.read("raw.json");
    let history = w.read("history.json");
    let quality = w.read("quality.json");
    let mut topics = w.read("topics.json");
    topics["metadata"]["input_sha256"] = json!(canonical_json_sha256(&raw).unwrap());
    let rules = json!({"min_support":1,"exclude_categories":["Maintenance"]});
    for supplements in [false, true] {
        w.run(7, supplements).unwrap();
        let bounded = w.read("result.json");
        let memory = analyze_memory_graph(
            &raw,
            &rules,
            supplements.then_some(&history),
            supplements.then_some(&quality),
            supplements.then_some(&topics),
            true,
        )
        .unwrap();
        assert_eq!(memory["pages"], bounded["pages"]);
        assert_eq!(memory["metadata"]["creators"]["unmatched_pages"], 523);
        assert_eq!(bounded["metadata"]["creators"]["unmatched_pages"], 0);
        assert!(memory["metadata"].get("input_checksum_format").is_none());
        assert_eq!(
            memory["metadata"]["input_sha256"],
            canonical_json_sha256(&raw).unwrap()
        );
        assert_eq!(
            memory["_incoming_sources"][1].as_array().unwrap().len(),
            131
        );
        if supplements {
            assert_eq!(memory["metadata"]["history"]["matched_pages"], 175);
            assert_eq!(
                memory["metadata"]["quality"]["input_sha256"],
                canonical_json_sha256(&quality).unwrap()
            );
        }
    }
}

#[test]
fn memory_analysis_rejects_partial_censuses_stale_supplements_and_external_enrichment() {
    let w = Workspace::new(6);
    let raw = w.read("raw.json");
    let rules = json!({});
    for mut changed in [raw.clone(), raw.clone(), raw.clone()]
        .into_iter()
        .enumerate()
    {
        match changed.0 {
            0 => changed.1["metadata"]["complete"] = json!(false),
            1 => changed.1["pages"][1]["page_id"] = changed.1["pages"][0]["page_id"].clone(),
            _ => changed.1["pages"][0]["biography"] = Value::Null,
        }
        assert!(analyze_memory_graph(&changed.1, &rules, None, None, None, false).is_err());
    }
    let mut quality = w.read("quality.json");
    quality["pages"][0]["quality_revision_relation"] = json!("different");
    assert!(analyze_memory_graph(&raw, &rules, None, Some(&quality), None, false).is_err());
    let mut history = w.read("history.json");
    history["metadata"]["sql_release"] = json!("20260901");
    assert!(analyze_memory_graph(&raw, &rules, Some(&history), None, None, false).is_err());
    assert!(analyze_memory_graph(
        &raw,
        &rules,
        None,
        None,
        Some(&w.read("topics.json")),
        false
    )
    .is_err());
}

#[test]
fn large_sql_comparison_builds_complete_eligible_edges_from_verified_staging() {
    use std::io::Write;
    let w = Workspace::new(523);
    let mut raw = w.read("raw.json");
    raw["metadata"]["item_mapping"] = json!({"source":"page_props.wikibase_item","complete":true});
    let path = w.0.join("raw.json");
    fs::write(
        &path,
        format!(
            "{{\"metadata\":{},\"pages\":{},\"redirects\":{},\"category_parents\":{}}}",
            raw["metadata"], raw["pages"], raw["redirects"], raw["category_parents"]
        ),
    )
    .unwrap();
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    let padding = [b' '; 65536];
    for _ in 0..2048 {
        file.write_all(&padding).unwrap();
    }
    drop(file);
    let mut db = Connection::open("").unwrap();
    comparison_schema(&db).unwrap();
    stage_comparison_graph(
        &path,
        &mut db,
        &AnalysisOptions {
            rules: &json!({}),
            work_dir: Some(&w.0),
            history: None,
            quality: None,
            topics: None,
            batch_size: 7,
        },
    )
    .unwrap();
    let count = |table: &str| -> usize {
        db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count("pages"), 523);
    assert_eq!(count("items"), 523);
    assert_eq!(count("edges"), 131);
    let meta: Value = serde_json::from_str(
        &db.query_row("SELECT metadata FROM wikis", [], |r| r.get::<_, String>(0))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(meta["analysis_source"], "verified_import_stage");
    assert_eq!(meta["input_checksum_format"], "raw_file_bytes_v1");
    assert_eq!(meta["input_sha256"], hash(&fs::read(path).unwrap()));
}

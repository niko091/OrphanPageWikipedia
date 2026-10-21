#![cfg(feature = "cli")]
use _native::dumps::{
    comparison_schema, export_comparison_member, match_comparison, stage_comparison_graph,
    AnalysisOptions,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orphanwiki-matching-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn stage(&self, db: &mut Connection, raw: &Value) {
        let language = raw["metadata"]["language"].as_str().unwrap();
        let path = self.0.join(format!("{language}-raw.json"));
        fs::write(&path, raw.to_string()).unwrap();
        stage_comparison_graph(&path, db, &options()).unwrap();
        db.execute(
            "INSERT INTO comparison_inputs VALUES(?,?,?,'{}')",
            params![
                language,
                format!("graph-{language}"),
                format!("analysis-{language}")
            ],
        )
        .unwrap();
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn options() -> AnalysisOptions<'static> {
    static RULES: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| json!({}));
    AnalysisOptions {
        rules: &RULES,
        work_dir: None,
        history: None,
        quality: None,
        topics: None,
        batch_size: 7,
    }
}
fn graph(language: &str, links: &[(usize, Vec<usize>)], count: usize) -> Value {
    json!({"metadata":{"language":language,"source":"Synthetic complete graph","complete":true,"demo":true,"started_at":"2026-01-01T00:00:00Z","finished_at":"2026-01-02T00:00:00Z","item_mapping":{"source":"page_props.wikibase_item","complete":true}},"pages":(1..=count).rev().map(|id|json!({"page_id":id,"title":format!("{language}-{id:05}"),"wikibase_item":format!("Q{id}"),"links":links.iter().find(|x|x.0==id).map(|x|x.1.iter().map(|id|format!("{language}-{id:05}")).collect::<Vec<_>>()).unwrap_or_default()})).collect::<Vec<_>>(),"redirects":{}})
}
fn candidate(db: &Connection, language: &str, id: i64) -> Value {
    serde_json::from_str(
        &db.query_row(
            "SELECT data FROM candidates WHERE language=? AND id=?",
            params![language, id],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap()
}
fn census(db: &Connection, table: &str) -> usize {
    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn all_ordered_pairs_keep_evidence_dead_end_rank_and_zero_outcomes() {
    let w = Workspace::new();
    let mut db = Connection::open("").unwrap();
    comparison_schema(&db).unwrap();
    w.stage(&mut db, &graph("vec", &[(2, vec![4])], 8));
    w.stage(&mut db, &graph("lmo", &[(2, vec![1, 1]), (3, vec![1])], 8));
    w.stage(&mut db, &graph("fr", &[(4, vec![1]), (2, vec![5])], 8));
    match_comparison(&mut db).unwrap();
    assert_eq!(census(&db, "comparison_pairs"), 6);
    let record = candidate(&db, "vec", 1);
    assert_eq!(record["status"], "compared");
    assert_eq!(record["candidate_count"], 3);
    let candidates = record["candidates"].as_array().unwrap();
    assert_eq!(
        candidates
            .iter()
            .map(|c| c["page_id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [3, 4, 2]
    );
    assert_eq!(candidates[0]["dead_end"], true);
    assert_eq!(candidates[2]["dead_end"], false);
    assert_eq!(
        candidates[2]["evidence"],
        json!([{"language":"lmo","source_page_id":2,"source_title":"lmo-00002","target_page_id":1,"target_title":"lmo-00001"}])
    );
    assert_eq!(candidate(&db, "vec", 8)["candidate_count"], 0);
    let metadata: Value = serde_json::from_str(
        &db.query_row(
            "SELECT metadata FROM comparison_metadata WHERE language='vec'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        metadata["comparison"]["reference_languages"],
        json!(["fr", "lmo"])
    );
    assert_eq!(metadata["comparison"]["collections"][0]["language"], "fr");
    let first: i64 = db
        .query_row(
            "SELECT id FROM pages WHERE language='vec' ORDER BY position LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(first, 8);
    db.execute_batch("CREATE TRIGGER no_repeated_matching BEFORE INSERT ON comparison_matches BEGIN SELECT RAISE(ABORT,'matching repeated');END;").unwrap();
    match_comparison(&mut db).unwrap();
    assert_eq!(candidate(&db, "vec", 1), record);
}

#[test]
fn ambiguous_missing_and_unavailable_items_remain_distinct_from_known_zero() {
    let w = Workspace::new();
    let mut db = Connection::open("").unwrap();
    comparison_schema(&db).unwrap();
    let mut local = graph("vec", &[], 8);
    for page in local["pages"].as_array_mut().unwrap() {
        page["wikibase_item"] = match page["page_id"].as_i64().unwrap() {
            1 => Value::Null,
            2 => json!("Q01"),
            3 | 4 => json!("Q99"),
            5 => json!("Q200"),
            _ => page["wikibase_item"].clone(),
        };
    }
    w.stage(&mut db, &local);
    w.stage(&mut db, &graph("lmo", &[], 8));
    match_comparison(&mut db).unwrap();
    for id in [1, 2] {
        let c = candidate(&db, "vec", id);
        assert_eq!(c["status"], "missing_item");
        assert!(c["candidate_count"].is_null());
    }
    for id in [3, 4] {
        assert_eq!(candidate(&db, "vec", id)["status"], "ambiguous_item");
    }
    let unavailable = candidate(&db, "vec", 5);
    assert_eq!(unavailable["status"], "counterpart_unavailable");
    assert!(unavailable["candidate_count"].is_null());
    assert_eq!(candidate(&db, "vec", 8)["candidate_count"], 0);
    assert_eq!(
        candidate(&db, "lmo", 3)["counterparts"][0]["status"],
        "missing_counterpart"
    );
    assert_eq!(census(&db, "comparison_pairs"), 2);
    assert_eq!(census(&db, "comparison_matches"), 0);
}

#[test]
fn exceptional_target_fanout_retains_every_candidate_and_reference_edge() {
    let w = Workspace::new();
    let mut db = Connection::open("").unwrap();
    comparison_schema(&db).unwrap();
    let count = 9002;
    w.stage(&mut db, &graph("vec", &[], count));
    let links = (2..=count)
        .map(|id| (id, vec![1, 1, id]))
        .collect::<Vec<_>>();
    for lang in ["lmo", "fr"] {
        w.stage(&mut db, &graph(lang, &links, count));
    }
    match_comparison(&mut db).unwrap();
    let record = candidate(&db, "vec", 1);
    let candidates = record["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), count - 1);
    assert_eq!(record["candidate_count"], count - 1);
    for c in candidates {
        assert_eq!(c["out_degree"], 0);
        let evidence = c["evidence"].as_array().unwrap();
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0]["language"], "fr");
        assert_eq!(evidence[1]["language"], "lmo");
    }
    assert_eq!(census(&db, "comparison_pairs"), 6);
}

#[test]
fn staging_rolls_back_late_invalid_ids_and_rejects_incomplete_mapping() {
    let w = Workspace::new();
    let mut db = Connection::open("").unwrap();
    comparison_schema(&db).unwrap();
    w.stage(&mut db, &graph("vec", &[], 8));
    let mut raw = graph("lmo", &[], 8);
    raw["pages"][7]["page_id"] = json!(0);
    let path = w.0.join("invalid.json");
    fs::write(&path, raw.to_string()).unwrap();
    assert!(stage_comparison_graph(&path, &mut db, &options()).is_err());
    assert_eq!(census(&db, "pages"), 8);
    assert_eq!(census(&db, "wikis"), 1);
    raw["pages"][7]["page_id"] = json!(1);
    raw["metadata"]["item_mapping"]["complete"] = json!(false);
    fs::write(&path, raw.to_string()).unwrap();
    assert!(stage_comparison_graph(&path, &mut db, &options()).is_err());
    assert!(match_comparison(&mut db).is_err());
    assert_eq!(census(&db, "comparison_pairs"), 0);
}

#[test]
fn comparison_export_streams_original_order_and_preserves_an_approved_file_on_failure() {
    let w = Workspace::new();
    let mut db = Connection::open("").unwrap();
    comparison_schema(&db).unwrap();
    w.stage(&mut db, &graph("vec", &[], 8));
    w.stage(&mut db, &graph("lmo", &[(2, vec![1])], 8));
    match_comparison(&mut db).unwrap();
    let output = w.0.join("canonical.json");
    export_comparison_member(&db, "vec", &output).unwrap();
    let bytes = fs::read(&output).unwrap();
    let result: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["pages"].as_array().unwrap().len(), 8);
    assert_eq!(result["pages"][0]["page_id"], 8);
    assert_eq!(result["link_candidates"]["1"], candidate(&db, "vec", 1));
    assert!(result["metadata"].get("comparison").is_some());
    db.execute("DELETE FROM pages WHERE language='vec' AND id=8", [])
        .unwrap();
    assert!(export_comparison_member(&db, "vec", &output).is_err());
    assert_eq!(fs::read(&output).unwrap(), bytes);
}

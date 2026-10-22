#![cfg(feature = "cli")]
use orphanwiki::dumps::{
    compare_files, comparison_schema, export_comparison_member, match_comparison,
    seed_comparison_pairs, stage_comparison_graph, AnalysisOptions, ComparisonOptions,
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

fn cache_options(work: &std::path::Path) -> ComparisonOptions<'_> {
    ComparisonOptions {
        rules: options().rules,
        work_dir: work,
        topics: &[],
        histories: &[],
        qualities: &[],
        allowed_languages: None,
        published: None,
    }
}
fn write_graph(w: &Workspace, raw: &Value) -> PathBuf {
    let path = w.0.join(format!(
        "{}-raw.json",
        raw["metadata"]["language"].as_str().unwrap()
    ));
    fs::write(&path, raw.to_string()).unwrap();
    path
}
fn cache_manifest(work: &std::path::Path) -> (Vec<u8>, PathBuf, Vec<u8>) {
    let bytes = fs::read(work.join("comparison-cache/manifest.json")).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let path = work
        .join("comparison-cache")
        .join(value["file"].as_str().unwrap());
    let database = fs::read(&path).unwrap();
    (bytes, path, database)
}
fn pairs(db: &Connection) -> Vec<(String, String, String, String)> {
    db.prepare("SELECT * FROM comparison_pairs ORDER BY language,reference")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
fn matches(db: &Connection) -> Vec<(String, String, i64, i64, i64, i64)> {
    db.prepare("SELECT * FROM comparison_matches ORDER BY language,reference,target,source")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn persistent_cache_reuses_generations_and_invalidates_only_incident_graph_pairs() {
    let w = Workspace::new();
    let work = w.0.join("work");
    let opts = cache_options(&work);
    let vec = write_graph(&w, &graph("vec", &[(2, vec![4])], 8));
    let lmo = write_graph(&w, &graph("lmo", &[(2, vec![1]), (3, vec![1])], 8));
    let db = compare_files(&[vec.clone(), lmo.clone()], &opts).unwrap();
    let initial = cache_manifest(&work);
    let original_pairs = pairs(&db);
    let original_matches = matches(&db);
    assert!(db.execute("DELETE FROM wikis", []).is_err());
    db.close().unwrap();
    let db = compare_files(std::slice::from_ref(&vec), &opts).unwrap();
    assert_eq!(cache_manifest(&work).0, initial.0);
    assert_eq!(pairs(&db), original_pairs);
    db.close().unwrap();
    let fr = write_graph(&w, &graph("fr", &[(4, vec![1])], 8));
    let db = compare_files(std::slice::from_ref(&fr), &opts).unwrap();
    assert_eq!(census(&db, "wikis"), 3);
    assert_eq!(census(&db, "comparison_pairs"), 6);
    assert_eq!(
        matches(&db)
            .into_iter()
            .filter(|r| r.0 != "fr" && r.1 != "fr")
            .collect::<Vec<_>>(),
        original_matches
    );
    assert_eq!(
        pairs(&db)
            .into_iter()
            .filter(|r| r.0 != "fr" && r.1 != "fr")
            .collect::<Vec<_>>(),
        original_pairs
    );
    let all_pairs = pairs(&db);
    let all_matches = matches(&db);
    let expanded = cache_manifest(&work);
    db.close().unwrap();
    let changed_rules = json!({"min_support":2});
    let mut measures = cache_options(&work);
    measures.rules = &changed_rules;
    let db = compare_files(std::slice::from_ref(&fr), &measures).unwrap();
    assert_eq!(pairs(&db), all_pairs);
    assert_eq!(matches(&db), all_matches);
    let refreshed = cache_manifest(&work);
    db.close().unwrap();
    write_graph(&w, &graph("lmo", &[(5, vec![1])], 8));
    let db = compare_files(std::slice::from_ref(&fr), &measures).unwrap();
    assert_eq!(
        pairs(&db)
            .into_iter()
            .filter(|r| r.0 != "lmo" && r.1 != "lmo")
            .collect::<Vec<_>>(),
        all_pairs
            .into_iter()
            .filter(|r| r.0 != "lmo" && r.1 != "lmo")
            .collect::<Vec<_>>()
    );
    assert_eq!(
        matches(&db)
            .into_iter()
            .filter(|r| r.0 != "lmo" && r.1 != "lmo")
            .collect::<Vec<_>>(),
        all_matches
            .into_iter()
            .filter(|r| r.0 != "lmo" && r.1 != "lmo")
            .collect::<Vec<_>>()
    );
    assert_ne!(candidate(&db, "vec", 1)["candidates"][0]["page_id"], 3);
    for old in [initial, expanded, refreshed] {
        assert_eq!(fs::read(old.1).unwrap(), old.2);
    }
    let sources: Vec<String> = db
        .prepare("SELECT sources FROM comparison_inputs")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for source in sources {
        let value: Value = serde_json::from_str(&source).unwrap();
        for path in value
            .as_object()
            .unwrap()
            .values()
            .filter_map(Value::as_str)
        {
            assert!(!std::path::Path::new(path).is_absolute());
        }
    }
}

#[test]
fn incomplete_cache_updates_and_corruption_cannot_approve_a_new_generation() {
    let w = Workspace::new();
    let work = w.0.join("work");
    let opts = cache_options(&work);
    let vec = write_graph(&w, &graph("vec", &[], 8));
    let lmo = write_graph(&w, &graph("lmo", &[(2, vec![1])], 8));
    compare_files(&[vec.clone(), lmo.clone()], &opts)
        .unwrap()
        .close()
        .unwrap();
    let approved = cache_manifest(&work);
    write_graph(&w, &graph("vec", &[(3, vec![4])], 8));
    let mut invalid = graph("lmo", &[], 8);
    invalid["pages"][7]["page_id"] = json!(0);
    write_graph(&w, &invalid);
    assert!(compare_files(std::slice::from_ref(&vec), &opts).is_err());
    assert_eq!(cache_manifest(&work).0, approved.0);
    assert_eq!(fs::read(&approved.1).unwrap(), approved.2);
    assert!(fs::read_dir(&work).unwrap().all(|p| !p
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("orphanwiki-export-")));
    write_graph(&w, &graph("lmo", &[(2, vec![1])], 8));
    fs::write(&approved.1, b"corrupt").unwrap();
    assert!(compare_files(std::slice::from_ref(&vec), &opts).is_err());
    assert_eq!(
        fs::read(work.join("comparison-cache/manifest.json")).unwrap(),
        approved.0
    );
}

#[test]
fn cache_retirement_obeys_the_configured_language_pool() {
    let w = Workspace::new();
    let work = w.0.join("work");
    let mut opts = cache_options(&work);
    let paths = [
        write_graph(&w, &graph("vec", &[], 8)),
        write_graph(&w, &graph("lmo", &[(2, vec![1])], 8)),
        write_graph(&w, &graph("fr", &[(3, vec![1])], 8)),
    ];
    compare_files(&paths, &opts).unwrap().close().unwrap();
    let pool = vec!["vec".to_owned(), "lmo".to_owned()];
    opts.allowed_languages = Some(&pool);
    assert!(compare_files(std::slice::from_ref(&paths[2]), &opts).is_err());
    let db = compare_files(std::slice::from_ref(&paths[0]), &opts).unwrap();
    assert_eq!(census(&db, "wikis"), 2);
    assert_eq!(census(&db, "comparison_pairs"), 2);
    assert!(pairs(&db).iter().all(|r| r.0 != "fr" && r.1 != "fr"));
}

#[test]
fn prior_publications_seed_only_verified_complete_pairs_and_reject_tampered_evidence() {
    let w = Workspace::new();
    let mut first = Connection::open("").unwrap();
    comparison_schema(&first).unwrap();
    let raws = [graph("vec", &[], 8), graph("lmo", &[(2, vec![1])], 8)];
    for raw in &raws {
        w.stage(&mut first, raw);
    }
    match_comparison(&mut first).unwrap();
    let mut published = std::collections::BTreeMap::new();
    for lang in ["vec", "lmo"] {
        let path = w.0.join(format!("{lang}.json"));
        export_comparison_member(&first, lang, &path).unwrap();
        published.insert(lang.to_owned(), path);
    }
    let mut next = Connection::open("").unwrap();
    comparison_schema(&next).unwrap();
    for raw in &raws {
        w.stage(&mut next, raw);
    }
    seed_comparison_pairs(&mut next, &published).unwrap();
    assert_eq!(pairs(&next), pairs(&first));
    assert_eq!(matches(&next), matches(&first));
    next.execute_batch("CREATE TRIGGER no_rematching BEFORE INSERT ON comparison_matches BEGIN SELECT RAISE(ABORT,'seed ignored');END;").unwrap();
    match_comparison(&mut next).unwrap();
    assert_eq!(candidate(&next, "vec", 1), candidate(&first, "vec", 1));
    let path = &published["vec"];
    let mut canonical: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    canonical["link_candidates"]["1"]["candidates"][0]["evidence"][0]["source_title"] =
        json!("Tampered");
    fs::write(path, canonical.to_string()).unwrap();
    let mut invalid = Connection::open("").unwrap();
    comparison_schema(&invalid).unwrap();
    for raw in &raws {
        w.stage(&mut invalid, raw);
    }
    assert!(seed_comparison_pairs(
        &mut invalid,
        &std::collections::BTreeMap::from([("vec".into(), path.clone())])
    )
    .is_err());
    assert_eq!(census(&invalid, "comparison_pairs"), 0);
    assert_eq!(census(&invalid, "comparison_matches"), 0);
}

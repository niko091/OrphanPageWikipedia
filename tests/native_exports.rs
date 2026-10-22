#![cfg(feature = "cli")]
use orphanwiki::dumps::{
    aggregate_index, comparison_filename, export_csv, prepare_dashboard_exports,
    publish_available_aggregate, rebuild_dashboard, write_aggregate_summary, ARTICLE_INDEX_COLUMNS,
    CSV_COLUMNS, OBSERVATION_COLUMNS,
};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orphanwiki-exports-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn source(&self, pages: &Value, candidates: Option<&Value>) -> PathBuf {
        self.canonical(
            &json!({"language":"vec","complete":true}),
            pages,
            candidates,
        )
    }
    fn canonical(&self, metadata: &Value, pages: &Value, candidates: Option<&Value>) -> PathBuf {
        let path = self.0.join("canonical.json");
        let mut text = format!("{{\"metadata\":{metadata},\"pages\":{pages}");
        if let Some(candidates) = candidates {
            text.push_str(&format!(",\"link_candidates\":{candidates}"));
        }
        text.push('}');
        fs::write(&path, text).unwrap();
        path
    }
    fn assert_files(&self, expected: &[&str]) {
        let mut names = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, expected);
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn header() -> String {
    // Frozen Python DictWriter header; independent of the native column list.
    concat!(
        "page_id,title,category,category_status,orphan,in_degree,out_degree,created_at,age_days,",
        "history_first_revision_at,history_first_revision_age_days,length_bytes,origin,creator_key,",
        "creator_data_status,creator_account_type,creator_tenure_days,creator_prior_edits_main,",
        "creator_prior_edits_other,creator_prior_articles,creator_articles_created_total,",
        "creator_articles_in_census,creator_orphans_current,creator_orphans_current_share,",
        "creator_orphans_at_30_days,history_creator_key,history_creator_status,history_data_status,",
        "creator_groups_at_creation_history,creator_continuity_months_before_creation,",
        "creator_continuity_bucket_before_creation,creator_prior_edits_main_history,",
        "creator_prior_edits_other_history,creator_attribution,creator_registered_at,",
        "creator_tenure_status,creator_experience_status,topic_labels,topic_status,topic_input_count,",
        "wikibase_item,candidate_count,candidate_status,page_created_at,first_public_revision_at,",
        "creation_date_source,creation_date_status,creator_account_type_at_creation,creator_is_bot_now,",
        "creator_languages_created,creator_languages_before,creation_tags,import_log_observed,",
        "move_log_observed,creator_orphans_at_30_days_status,quality_score,quality_status,",
        "content_revision_id,quality_revision_relation\r\n"
    )
    .into()
}

#[test]
fn dashboard_publication_switches_generations_and_keeps_every_previously_approved_file() {
    let w = Workspace::new();
    let source = w.source(
        &json!([{"page_id":1,"title":"A","orphan":true},{"page_id":2,"title":"B","orphan":false}]),
        None,
    );
    fs::write(w.0.join("canonical.csv"), "approved csv").unwrap();
    let aggregate = json!({"file":"all.json","language":"all","kind":"aggregate","date":null});
    fs::write(w.0.join("all.json"), "approved aggregate").unwrap();
    fs::write(w.0.join("manifest.json"),json!([{"file":"canonical.json","language":"vec","date":null,"published_at":"old"},aggregate]).to_string()).unwrap();
    let canonical = fs::read(&source).unwrap();
    rebuild_dashboard(&w.0).unwrap();
    let first = read_json(w.0.join("manifest.json"));
    let old = snapshot(&w.0);
    assert_eq!(first[1], aggregate);
    assert_eq!(first[0]["published_at"], "old");
    assert_eq!(first[0].as_object().unwrap().len(), 13);
    for (key, name) in first[0].as_object().unwrap() {
        if key.ends_with("_file") || key.ends_with("_dir") {
            assert!(w.0.join(name.as_str().unwrap()).exists());
        }
    }
    rebuild_dashboard(&w.0).unwrap();
    let next = read_json(w.0.join("manifest.json"));
    for key in [
        "observations_file",
        "articles_file",
        "summary_file",
        "creator_index_file",
        "cards_file",
        "cards_dir",
        "connections_dir",
        "connections_metadata_file",
        "profile_topics_dir",
    ] {
        assert_ne!(first[0][key], next[0][key]);
    }
    for (path, bytes) in old {
        if path != std::path::Path::new("manifest.json") {
            assert_eq!(fs::read(w.0.join(path)).unwrap(), bytes);
        }
    }
    assert_eq!(fs::read(&source).unwrap(), canonical);
    assert_eq!(
        fs::read_to_string(w.0.join("canonical.csv")).unwrap(),
        "approved csv"
    );
}

#[test]
fn invalid_dashboard_members_and_duplicate_manifests_publish_nothing() {
    let w = Workspace::new();
    w.source(&json!([{"page_id":1,"title":"A","orphan":true}]), None);
    fs::write(w.0.join("broken.json"), "{\"metadata\":").unwrap();
    let good = json!({"file":"canonical.json","language":"vec","date":null});
    for manifest in [
        json!([good,{"file":"broken.json","language":"lmo","date":null}]),
        json!([good, good]),
        json!([{"file":"../bad.json","language":"vec"}]),
        json!([]),
    ] {
        fs::write(w.0.join("manifest.json"), manifest.to_string()).unwrap();
        let before = snapshot(&w.0);
        assert!(rebuild_dashboard(&w.0).is_err());
        assert_eq!(snapshot(&w.0), before);
    }
}

#[test]
fn csv_bytes_preserve_column_order_ages_missing_zero_and_candidate_projection() {
    let w = Workspace::new();
    let pages = json!([
        {"page_id":7,"title":"È, \"🌙\"\r\nline","orphan":true,
         "created_at":null,"age_days":null,"history_first_revision_at":"2020-01-01T00:00:00Z",
         "history_first_revision_age_days":42.5,"length_bytes":0,"in_degree":0,"out_degree":2,
         "creator_groups_at_creation_history":["sysop","reviewer"],"creator_is_bot_now":false,
         "quality_score":0.9859704508633369,"content_revision_id":9223372036854775807i64,
         "candidate_count":999,"candidate_status":"ignored","extra":"ignored"},
        {"page_id":2,"title":"Empty topics","orphan":true,"topic_labels":[],
         "topic_status":"predicted","topic_input_count":0,"quality_score":0.0},
        {"page_id":5,"title":"Missing topics","orphan":true,"topic_labels":null},
        {"page_id":1,"title":"Non orphan","orphan":false}
    ]);
    let source = w.source(
        &pages,
        Some(
            &json!({"7":{"status":"compared","candidate_count":0,"candidates":[]},
                     "5":{"status":"missing_item","candidate_count":null}}),
        ),
    );
    let before = fs::read(&source).unwrap();
    let output = w.0.join("result.csv");
    assert_eq!(export_csv(&source, &output).unwrap(), 4);
    assert_eq!(CSV_COLUMNS.len(), 59);
    let mut first = vec![""; 59];
    for (index, value) in [
        (0, "7"),
        (1, "\"È, \"\"🌙\"\"\r\nline\""),
        (4, "True"),
        (5, "0"),
        (6, "2"),
        (9, "2020-01-01T00:00:00Z"),
        (10, "42.5"),
        (11, "0"),
        (28, "\"['sysop', 'reviewer']\""),
        (41, "0"),
        (42, "compared"),
        (48, "False"),
        (55, "0.9859704508633369"),
        (57, "9223372036854775807"),
    ] {
        first[index] = value;
    }
    let mut second = vec![""; 59];
    for (index, value) in [
        (0, "2"),
        (1, "Empty topics"),
        (4, "True"),
        (37, "[]"),
        (38, "predicted"),
        (39, "0"),
        (55, "0.0"),
    ] {
        second[index] = value;
    }
    let mut third = vec![""; 59];
    third[0] = "5";
    third[1] = "Missing topics";
    third[4] = "True";
    third[42] = "missing_item";
    let mut fourth = vec![""; 59];
    fourth[0] = "1";
    fourth[1] = "Non orphan";
    fourth[4] = "False";
    let expected = header()
        + &[first, second, third, fourth]
            .map(|row| row.join(",") + "\r\n")
            .concat();
    assert_eq!(fs::read(&output).unwrap(), expected.as_bytes());
    assert_eq!(fs::read(&source).unwrap(), before);
    w.assert_files(&["canonical.json", "result.csv"]);
}

#[test]
fn python_array_repr_and_float_notation_are_retained_inside_csv_fields() {
    let w = Workspace::new();
    let groups = json!([
        "l'autre",
        "a\"b",
        "both'\"",
        "\\\t\n\r",
        "\u{7f}\u{85}\u{a0}",
        "\u{200b}\u{2028}\u{e000}\u{10ffff}",
        "é 🌙",
        [null, false, true, -0.0, 1e-7, 1e16, 1255183068570128.2]
    ]);
    let source = w.source(
        &json!([{"page_id":1,"title":"Array","orphan":true,"topic_labels":groups}]),
        None,
    );
    let output = w.0.join("result.csv");
    export_csv(&source, &output).unwrap();
    // Python str(list) before the outer CSV escaping.
    let expected_repr = concat!(
        "[\"l'autre\", 'a\"b', 'both\\\'\"', '\\\\\\t\\n\\r', '\\x7f\\x85\\xa0', ",
        "'\\u200b\\u2028\\ue000\\U0010ffff', 'é 🌙', ",
        "[None, False, True, -0.0, 1e-07, 1e+16, 1255183068570128.2]]"
    );
    let mut fields = vec![String::new(); 59];
    fields[0] = "1".into();
    fields[1] = "Array".into();
    fields[4] = "True".into();
    fields[37] = format!("\"{}\"", expected_repr.replace('"', "\"\""));
    assert_eq!(
        fs::read_to_string(&output).unwrap(),
        header() + &fields.join(",") + "\r\n"
    );
}

#[test]
fn large_records_and_noncontiguous_ids_keep_source_order_without_evidence_loss() {
    let w = Workspace::new();
    let pages = (0..1103)
        .rev()
        .map(|i| json!({"page_id":i*3+1,"title":format!("Title {i}"),"orphan":true}))
        .collect::<Vec<_>>();
    let evidence = (0..6001)
        .map(|i| json!({"language":"lmo","source_page_id":i+1,"source_title":"È 🌙"}))
        .collect::<Vec<_>>();
    let source = w.source(
        &json!(pages),
        Some(
            &json!({"1":{"candidate_count":1,"status":"compared","candidates":[
            {"page_id":4,"title":"x".repeat(260_000),"evidence":evidence}]}}),
        ),
    );
    let before = fs::read(&source).unwrap();
    let output = w.0.join("result.csv");
    assert_eq!(export_csv(&source, &output).unwrap(), 1103);
    let result = fs::read_to_string(&output).unwrap();
    let lines = result.split("\r\n").collect::<Vec<_>>();
    assert_eq!(lines.len(), 1105);
    assert!(lines[1].starts_with("3307,Title 1102,,,True,"));
    assert!(lines[1103].starts_with("1,Title 0,,,True,"));
    assert_eq!(lines[1103].split(',').nth(41), Some("1"));
    assert_eq!(lines[1103].split(',').nth(42), Some("compared"));
    assert_eq!(fs::read(&source).unwrap(), before);
    w.assert_files(&["canonical.json", "result.csv"]);
}

#[test]
fn malformed_sections_and_duplicate_or_foreign_ids_preserve_the_approved_csv() {
    let w = Workspace::new();
    let source = w.0.join("canonical.json");
    let output = w.0.join("result.csv");
    let metadata = r#""metadata":{"language":"vec","complete":true}"#;
    let page = r#"{"page_id":1,"title":"A","orphan":true}"#;
    let invalid = vec![
        r#"{"pages":[],"metadata":{}}"#.into(),
        format!("{{{metadata}}}"),
        format!("{{{metadata},\"pages\":[0]}}"),
        format!("{{{metadata},\"pages\":[{page},{page}]}}"),
        format!("{{{metadata},\"pages\":[{page}],\"pages\":[]}}"),
        format!("{{{metadata},\"pages\":[{page}],\"extra\":[]}}"),
        format!("{{{metadata},\"pages\":[{page}]}} trailing"),
        format!("{{{metadata},\"pages\":[{page}],\"link_candidates\":[]}}"),
        format!("{{{metadata},\"pages\":[{page}],\"link_candidates\":{{\"1\":0}}}}"),
        format!("{{{metadata},\"pages\":[{page}],\"link_candidates\":{{\"1\":{{}},\"1\":{{}}}}}}"),
        format!("{{{metadata},\"pages\":[{page}],\"link_candidates\":{{\"2\":{{}}}}}}"),
        format!("{{{metadata},\"pages\":[{page}],\"link_candidates\":{{\"01\":{{}}}}}}"),
        format!("{{{metadata},\"pages\":[{page}],\"link_candidates\":{{\"1\":{{\"candidate_count\":-1}}}}}}"),
        format!("{{{metadata},\"pages\":[{page}],\"link_candidates\":{{\"1\":{{\"candidate_count\":true}}}}}}"),
        format!("{{{metadata},\"pages\":[{page}],\"link_candidates\":{{\"1\":{{\"status\":[]}}}}}}"),
    ];
    for text in invalid {
        fs::write(&source, &text).unwrap();
        fs::write(&output, "approved").unwrap();
        assert!(export_csv(&source, &output).is_err(), "accepted {text}");
        let prepared = w.0.join("prepared");
        assert!(prepare_dashboard_exports(&source, &prepared, None, None).is_err());
        assert!(!prepared.exists());
        assert_eq!(fs::read_to_string(&output).unwrap(), "approved");
        assert_eq!(fs::read_to_string(&source).unwrap(), text);
        w.assert_files(&["canonical.json", "result.csv"]);
    }
    for page in [
        json!({"page_id":true,"orphan":true}),
        json!({"page_id":0,"orphan":true}),
        json!({"page_id":1.0,"orphan":true}),
        json!({"page_id":1,"orphan":0}),
        json!({"page_id":1,"orphan":true,"biography":null}),
        json!({"page_id":1,"orphan":true,"gender_group":null}),
    ] {
        w.source(&json!([page]), None);
        assert!(export_csv(&source, &output).is_err());
        assert_eq!(fs::read_to_string(&output).unwrap(), "approved");
    }
    w.source(
        &json!([{"page_id":1,"orphan":false}]),
        Some(&json!({"1":{}})),
    );
    assert!(export_csv(&source, &output).is_err());
}

#[test]
fn late_decode_and_csv_encoding_failures_clean_temporary_outputs() {
    let w = Workspace::new();
    let pages = json!([
        {"page_id":1,"title":"x".repeat(260_000),"orphan":true},
        {"page_id":2,"title":"Invalid","orphan":true,"topic_labels":[{"invalid":"object"}]}
    ]);
    let source = w.source(&pages, None);
    let output = w.0.join("result.csv");
    fs::write(&output, "approved").unwrap();
    assert!(export_csv(&source, &output).is_err());
    assert_eq!(fs::read_to_string(&output).unwrap(), "approved");
    w.assert_files(&["canonical.json", "result.csv"]);
    w.source(
        &json!([{"page_id":1,"title":"x".repeat(260_000),"orphan":true}]),
        None,
    );
    let mut text = fs::read_to_string(&source).unwrap();
    text.pop();
    fs::write(&source, &text).unwrap();
    assert!(export_csv(&source, &output).is_err());
    assert_eq!(fs::read_to_string(&output).unwrap(), "approved");
    w.assert_files(&["canonical.json", "result.csv"]);
}

#[test]
fn empty_census_missing_candidate_section_and_publication_failures_follow_contract() {
    let w = Workspace::new();
    let source = w.source(&json!([]), None);
    let output = w.0.join("result.csv");
    assert_eq!(export_csv(&source, &output).unwrap(), 0);
    assert_eq!(fs::read_to_string(&output).unwrap(), header());
    let before = fs::read(&source).unwrap();
    assert!(export_csv(&source, &source).is_err());
    assert_eq!(fs::read(&source).unwrap(), before);
    fs::create_dir(w.0.join("directory.csv")).unwrap();
    fs::write(w.0.join("directory.csv/approved"), "approved").unwrap();
    assert!(export_csv(&source, &w.0.join("directory.csv")).is_err());
    assert_eq!(
        fs::read_to_string(w.0.join("directory.csv/approved")).unwrap(),
        "approved"
    );
    w.assert_files(&["canonical.json", "directory.csv", "result.csv"]);
    for text in [
        r#"{"metadata":{"language":"vec","complete":false},"pages":[]}"#,
        r#"{"metadata":{"language":"all","complete":true},"pages":[]}"#,
        r#"{"metadata":{"language":"vec","complete":true,"biographies":null},"pages":[]}"#,
    ] {
        fs::write(&source, text).unwrap();
        assert!(export_csv(&source, &output).is_err());
        assert_eq!(fs::read_to_string(&output).unwrap(), header());
    }
}

#[cfg(unix)]
#[test]
fn an_output_symlink_cannot_replace_the_canonical_source() {
    let w = Workspace::new();
    let source = w.source(&json!([]), None);
    let alias = w.0.join("alias.csv");
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    let before = fs::read(&source).unwrap();
    assert!(export_csv(&source, &alias).is_err());
    assert_eq!(fs::read(&source).unwrap(), before);
    assert!(fs::symlink_metadata(&alias).unwrap().is_symlink());
}

fn read_json(path: impl AsRef<std::path::Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn browser_indices_keep_every_display_value_candidate_count_and_topic_lookup() {
    let w = Workspace::new();
    let pages = (1..=2103).rev().map(|id| json!({
        "page_id":id,"title":format!("È 🌙 {id}"),"orphan":id%3!=0,
        "category":if id%4==0 {Some("Specific")} else {None},
        "created_at":null,"page_created_at":null,"age_days":null,
        "history_first_revision_at":"2000-01-01T00:00:00Z","history_first_revision_age_days":42.5,
        "creator_is_bot_now":false,"length_bytes":0,"in_degree":if id%3==0 {1} else {0},
        "topic_status":if id%3==0 {"missing"} else {"predicted"},
        "topic_labels":if id%3==0 {Value::Null} else if id%3==1 {json!([])} else {json!(["Culture.Media.Music"])},
        "quality_score":0.9859704508633369,"quality_revision_relation":"different",
        "creator_groups_at_creation_history":["sysop","bot"],
        "ignored":"not a display column"
    })).collect::<Vec<_>>();
    let candidates = json!({"2":{"status":"compared","candidate_count":0,"candidates":[]},
    "5":{"status":"missing_item","candidate_count":null},
    "1024":{"status":"compared","candidate_count":1,"candidates":[{
        "page_id":1,"title":"huge".repeat(65000),"evidence":(0..6001).map(|i|json!({"source_page_id":i+1})).collect::<Vec<_>>()
    }]}});
    let source = w.source(&json!(pages), Some(&candidates));
    let before = fs::read(&source).unwrap();
    let folder = w.0.join("prepared");
    let names = prepare_dashboard_exports(&source, &folder, None, None).unwrap();
    assert_eq!(names.as_object().unwrap().len(), 9);
    assert!(!folder.join("manifest.json").exists());
    for (key, format, columns) in [
        ("observations_file", "observations_v1", OBSERVATION_COLUMNS),
        ("articles_file", "article_index_v1", ARTICLE_INDEX_COLUMNS),
    ] {
        let output = folder.join(names[key].as_str().unwrap());
        let index = read_json(&output);
        assert_eq!(index["format"], format);
        assert_eq!(index["metadata"], read_json(&source)["metadata"]);
        assert_eq!(index["columns"], json!(columns));
        assert_eq!(index["rows"].as_array().unwrap().len(), pages.len());
        for (page, row) in pages.iter().zip(index["rows"].as_array().unwrap()) {
            assert_eq!(
                *row,
                json!(columns.iter().map(|key| &page[key]).collect::<Vec<_>>())
            );
        }
        assert_eq!(
            index["link_candidates"],
            json!({"2":{"status":"compared","candidate_count":0},
            "5":{"status":"missing_item","candidate_count":null},
            "1024":{"status":"compared","candidate_count":1}})
        );
        assert!(!fs::read_to_string(&output)
            .unwrap()
            .contains("source_page_id"));
        if key == "articles_file" {
            assert_eq!(index["census_page_count"], pages.len());
        } else {
            assert!(index.get("census_page_count").is_none());
        }
    }
    let directory = folder.join(names["profile_topics_dir"].as_str().unwrap());
    let index = read_json(directory.join("index.json"));
    assert_eq!(index["format"], "profile_topics_index_v1");
    assert_eq!(index["shards"], json!([0, 1, 2]));
    assert_eq!(index["shard_size"], 1024);
    assert_eq!(index["census_page_count"], pages.len());
    let mut seen = 0;
    for (bucket, count) in [(0, 1023), (1, 1024), (2, 56)] {
        let shard = read_json(directory.join(format!("{bucket}.json")));
        assert_eq!(shard["format"], "profile_topics_shard_v1");
        assert_eq!(shard["metadata"], index["metadata"]);
        assert_eq!(shard["bucket"], bucket);
        assert_eq!(shard["rows"].as_array().unwrap().len(), count);
        for row in shard["rows"].as_array().unwrap() {
            seen += 1;
            let id = row[0].as_u64().unwrap() as usize;
            assert_eq!(id, seen);
            let page = &pages[pages.len() - id];
            assert_eq!(
                *row,
                json!([id, page["topic_status"], page["topic_labels"]])
            );
        }
    }
    assert_eq!(seen, pages.len());
    assert_eq!(fs::read(&source).unwrap(), before);
}

fn snapshot(folder: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn collect(
        root: &std::path::Path,
        folder: &std::path::Path,
        items: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) {
        for entry in fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(root, &path, items);
            } else {
                items.insert(
                    path.strip_prefix(root).unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut items = std::collections::BTreeMap::new();
    collect(folder, folder, &mut items);
    items
}

#[test]
fn source_fingerprints_and_explicit_generations_preserve_all_prior_index_shard_pairs() {
    let w = Workspace::new();
    let source=w.source(&json!([
        {"page_id":9000,"title":"Known negative","orphan":true,"topic_status":"predicted","topic_labels":[]},
        {"page_id":1024,"title":"Missing","orphan":true,"topic_status":"missing","topic_labels":null},
        {"page_id":1,"title":"Known topic","orphan":false,"topic_status":"predicted","topic_labels":["Culture.Media.Music"]}
    ]),None);
    let first = w.0.join("first");
    let old = prepare_dashboard_exports(&source, &first, None, None).unwrap();
    let frozen = snapshot(&first);
    let same = w.0.join("same");
    assert_eq!(
        prepare_dashboard_exports(&source, &same, None, None).unwrap(),
        old
    );
    assert_eq!(snapshot(&same), frozen);
    let modified =
        fs::metadata(&source).unwrap().modified().unwrap() + std::time::Duration::from_secs(1);
    fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let next = w.0.join("next");
    let changed = prepare_dashboard_exports(&source, &next, None, None).unwrap();
    for key in old.as_object().unwrap().keys() {
        assert_ne!(old[key], changed[key]);
    }
    assert_eq!(snapshot(&first), frozen);
    let explicit = w.0.join("explicit");
    let names = prepare_dashboard_exports(
        &source,
        &explicit,
        None,
        Some("0123456789abcdef0123456789abcdef"),
    )
    .unwrap();
    for name in names.as_object().unwrap().values() {
        assert!(name
            .as_str()
            .unwrap()
            .starts_with("canonical-0123456789abcdef0123456789abcdef-"));
    }
    let index = read_json(
        explicit
            .join(names["profile_topics_dir"].as_str().unwrap())
            .join("index.json"),
    );
    assert_eq!(index["shards"], json!([0, 1, 8]));
    assert_eq!(snapshot(&first), frozen);
}

#[test]
fn manifest_mismatches_invalid_generations_and_existing_directories_do_not_write() {
    let w = Workspace::new();
    let source = w.source(&json!([]), None);
    let prepared = w.0.join("prepared");
    for expected in [
        json!({"language":"lmo","date":null}),
        json!({"language":"vec","date":"different"}),
        json!({"language":"vec","date":null,"demo":true}),
    ] {
        assert!(prepare_dashboard_exports(&source, &prepared, Some(&expected), None).is_err());
        assert!(!prepared.exists());
    }
    for generation in [
        "",
        "a",
        "ABCDEF0123456789abcdef0123456789ab",
        "../0123456789abcdef0123456789abcd",
        "g123456789abcdef0123456789abcdef",
    ] {
        assert!(prepare_dashboard_exports(&source, &prepared, None, Some(generation)).is_err());
        assert!(!prepared.exists());
    }
    fs::create_dir(&prepared).unwrap();
    fs::write(prepared.join("approved"), "old").unwrap();
    assert!(prepare_dashboard_exports(&source, &prepared, None, None).is_err());
    assert_eq!(
        fs::read_to_string(prepared.join("approved")).unwrap(),
        "old"
    );
    assert!(prepare_dashboard_exports(&source, &source, None, None).is_err());
    assert!(prepare_dashboard_exports(&source, &w.0, None, None).is_err());
    fs::create_dir(w.0.join("empty")).unwrap();
    assert!(prepare_dashboard_exports(&source, &w.0.join("empty"), None, None).is_err());
    assert!(w.0.join("empty").is_dir());
    let accepted = w.0.join("accepted");
    assert!(prepare_dashboard_exports(
        &source,
        &accepted,
        Some(&json!({"language":"vec","date":null,"demo":false})),
        None
    )
    .is_ok());
}

#[test]
fn empty_browser_indices_retain_the_census_and_a_complete_empty_topic_directory() {
    let w = Workspace::new();
    let source = w.source(&json!([]), Some(&json!({})));
    let folder = w.0.join("prepared");
    let names = prepare_dashboard_exports(&source, &folder, None, None).unwrap();
    for key in ["observations_file", "articles_file"] {
        let index = read_json(folder.join(names[key].as_str().unwrap()));
        assert_eq!(index["rows"], json!([]));
        assert_eq!(index["link_candidates"], json!({}));
    }
    let directory = folder.join(names["profile_topics_dir"].as_str().unwrap());
    let index = read_json(directory.join("index.json"));
    assert_eq!(index["census_page_count"], 0);
    assert_eq!(index["shards"], json!([]));
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    for key in ["cards_dir", "connections_dir"] {
        let path = folder.join(names[key].as_str().unwrap());
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(path).unwrap().count(), 0);
    }
    let cards = read_json(folder.join(names["cards_file"].as_str().unwrap()));
    assert_eq!(cards["census_page_count"], 0);
    assert_eq!(cards["rows"], json!([]));
    assert_eq!(cards["link_candidates"], json!({}));
}

#[test]
fn editorial_cards_bound_shortlists_while_details_keep_all_candidates_and_reference_edges() {
    let w = Workspace::new();
    let pages = json!([
        {"page_id":29,"title":"Explicit empty record","orphan":true},
        {"page_id":17,"title":"È 🌙 target","orphan":true,"topic_labels":[],"topic_status":"predicted"},
        {"page_id":1,"title":"Non orphan","orphan":false},
        {"page_id":23,"title":"No comparison","orphan":true}
    ]);
    let valid_edge = json!({"language":"lmo","source_page_id":101,"source_title":"Source È 🌙",
        "target_page_id":301,"target_title":"Reference target","extra":0.9859704508633369});
    let mut candidates = vec![
        json!({"page_id":17,"title":"Self","evidence":[valid_edge]}),
        json!({"page_id":true,"title":"Boolean","evidence":[valid_edge]}),
        json!({"page_id":1.0,"title":"Float","evidence":[valid_edge]}),
        json!({"page_id":0,"title":"Zero","evidence":[valid_edge]}),
        json!({"page_id":91,"title":"\u{1c}\u{1d}\u{1e}\u{1f}","evidence":[valid_edge]}),
        json!({"page_id":92,"title":"No evidence"}),
        json!({"page_id":93,"title":"Unsafe evidence","evidence":[
            {"language":"../bad","source_title":"A","target_title":"B"},
            {"language":"vec","source_title":"A","target_title":"B"},
            {"language":"all","source_title":"A","target_title":"B"},
            {"language":"lmo\n","source_title":"A","target_title":"B"},
            {"language":"lmo","source_title":"\u{1c}","target_title":"B"}]}),
    ];
    for i in 0..20 {
        let mut evidence = vec![
            json!({"language":"lmo","source_title":" ","target_title":"invalid"}),
            valid_edge.clone(),
        ];
        if i == 0 {
            evidence.extend((0..6001).map(|j|json!({"language":"fur","source_page_id":j+1,
                "source_title":"x".repeat(40),"target_page_id":301,"target_title":"Reference target"})));
        }
        candidates.push(
            json!({"page_id":100+i,"title":format!("Candidate {i}"),"dead_end":i<8,
            "out_degree":if i<8 {0} else {2},"evidence":evidence,"additional":"retained"}),
        );
        if i == 0 {
            candidates
                .push(json!({"page_id":100,"title":"Duplicate selected","evidence":[valid_edge]}));
        }
    }
    // Saturated groups must not inspect discarded evidence.
    candidates.push(json!({"page_id":999,"title":"Already full","dead_end":true,"evidence":null}));
    let record = json!({"status":"compared","candidate_count":candidates.len(),"candidates":candidates,
        "counterparts":[{"language":"lmo","status":"non_orphan"}],"extra":"full detail retained"});
    let source = w.source(&pages, Some(&json!({"17":record,"29":{}})));
    let before = fs::read(&source).unwrap();
    let folder = w.0.join("prepared");
    let names = prepare_dashboard_exports(&source, &folder, None, None).unwrap();
    let cards = read_json(folder.join(names["cards_file"].as_str().unwrap()));
    let metadata = read_json(folder.join(names["connections_metadata_file"].as_str().unwrap()));
    assert_eq!(cards["format"], "orphan_cards_v2");
    assert_eq!(cards["census_page_count"], 4);
    assert_eq!(
        cards["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row[0].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![29, 17, 23]
    );
    assert_eq!(
        cards["link_candidates"]["17"]["candidate_ids"],
        json!([100, 101, 102, 103, 104, 108, 109, 110, 111, 112])
    );
    assert_eq!(
        cards["link_candidates"]["29"],
        json!({"status":null,"candidate_count":null,"candidate_ids":[]})
    );
    assert!(cards["link_candidates"].get("23").is_none());
    assert_eq!(metadata["format"], "connections_metadata_v1");
    assert_eq!(metadata["census_page_count"], 4);
    assert_eq!(metadata["metadata"], read_json(&source)["metadata"]);
    assert_eq!(metadata["connection_identity"].as_str().unwrap().len(), 64);
    for id in [17, 23, 29] {
        let card = read_json(
            folder
                .join(names["cards_dir"].as_str().unwrap())
                .join(format!("{id}.json")),
        );
        let detail = read_json(
            folder
                .join(names["connections_dir"].as_str().unwrap())
                .join(format!("{id}.json")),
        );
        assert_eq!(card["format"], "orphan_card_v1");
        assert_eq!(card["card_identity"], cards["card_identity"]);
        assert_eq!(card["census_page_count"], 4);
        assert_eq!(detail["format"], "connections_v2");
        assert_eq!(
            detail["connection_identity"],
            metadata["connection_identity"]
        );
        assert_eq!(detail["census_page_count"], 4);
        assert!(detail.get("metadata").is_none());
        assert_eq!(detail["pages"], json!([card["page"]]));
        if id == 17 {
            assert_eq!(detail["link_candidates"]["17"], record);
            let selected = card["candidate_record"]["candidates"].as_array().unwrap();
            assert_eq!(selected.len(), 10);
            for candidate in selected {
                assert_eq!(candidate["evidence"], json!([valid_edge]));
                assert_eq!(candidate["additional"], "retained");
            }
        } else {
            assert_eq!(detail["link_candidates"][id.to_string()], json!({}));
            if id == 23 {
                assert!(card["candidate_record"].is_null());
            } else {
                assert_eq!(
                    card["candidate_record"],
                    json!({"status":null,"candidate_count":null,"candidates":[]})
                );
            }
        }
    }
    for key in ["cards_dir", "connections_dir"] {
        let directory = folder.join(names[key].as_str().unwrap());
        assert_eq!(fs::read_dir(directory).unwrap().count(), 3);
    }
    assert_eq!(fs::read(&source).unwrap(), before);
}

#[test]
fn editorial_parse_failures_after_an_earlier_detail_remove_all_private_products() {
    let w = Workspace::new();
    let pages =
        json!([{"page_id":1,"title":"A","orphan":true},{"page_id":2,"title":"B","orphan":true}]);
    for record in [
        json!({"candidates":null}),
        json!({"candidates":{}}),
        json!({"candidates":[0]}),
        json!({"candidates":[{"page_id":3,"title":"Valid","evidence":null}]}),
        json!({"candidates":[{"page_id":3,"title":"Valid","evidence":[0]}]}),
    ] {
        let source = w.source(
            &pages,
            Some(
                &json!({"1":{"status":"compared","candidate_count":0,"candidates":[]},"2":record}),
            ),
        );
        let before = fs::read(&source).unwrap();
        let folder = w.0.join("prepared");
        assert!(prepare_dashboard_exports(&source, &folder, None, None).is_err());
        assert!(!folder.exists());
        assert_eq!(fs::read(&source).unwrap(), before);
        w.assert_files(&["canonical.json"]);
    }
}

#[test]
fn a_census_without_orphans_still_prepares_empty_editorial_directories() {
    let w = Workspace::new();
    let source = w.source(
        &json!([{"page_id":1,"title":"A","orphan":false},{"page_id":2,"title":"B","orphan":false}]),
        None,
    );
    let folder = w.0.join("prepared");
    let names = prepare_dashboard_exports(&source, &folder, None, None).unwrap();
    let cards = read_json(folder.join(names["cards_file"].as_str().unwrap()));
    assert_eq!(cards["census_page_count"], 2);
    assert_eq!(cards["rows"], json!([]));
    assert_eq!(cards["link_candidates"], json!({}));
    for key in ["cards_dir", "connections_dir"] {
        assert_eq!(
            fs::read_dir(folder.join(names[key].as_str().unwrap()))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn prepared_summaries_keep_historical_attribution_creation_missing_values_and_source_identity() {
    let w = Workspace::new();
    let pages = json!([
        {"page_id":1,"title":"A","orphan":true,"page_created_at":"2020-01-01T00:00:00Z","age_days":365.25,
            "creator_key":"current:1","history_creator_key":"history:7","creator_prior_edits_main":999,
            "creator_prior_edits_main_history":3,"creator_groups_at_creation_history":["sysop"],"creator_is_bot_now":false,
            "topic_status":"predicted","topic_labels":[],"quality_status":"scored","quality_score":0.9859704508633369,"length_bytes":0},
        {"page_id":2,"title":"B","orphan":false,"page_created_at":null,"age_days":9000,
            "history_first_revision_age_days":9000,"creator_key":"current:2","history_creator_key":"history:7",
            "creator_prior_edits_main_history":5,"topic_status":"missing","topic_labels":null,"quality_status":"missing"}
    ]);
    let metadata = json!({"language":"vec","complete":true,"finished_at":"2026-10-02T00:00:00Z",
        "creators":{"status":"not_collected"},"history":{"status":"collected"},"measures_rule":"creation_quality_creators_v2"});
    let source = w.canonical(&metadata, &pages, None);
    let before = fs::read(&source).unwrap();
    let folder = w.0.join("historical");
    let names = prepare_dashboard_exports(&source, &folder, None, None).unwrap();
    let summary = read_json(folder.join(names["summary_file"].as_str().unwrap()));
    let creators = read_json(folder.join(names["creator_index_file"].as_str().unwrap()));
    assert_eq!(summary["format"], "dashboard_summary_v1");
    assert_eq!(summary["histogram_rule"], "shared_p2_p98_tails_v1");
    assert_eq!(summary["measures_rule"], "creation_quality_creators_v2");
    assert_eq!(summary["metadata"], metadata);
    assert_eq!(creators["metadata"], metadata);
    assert_eq!(summary["census_page_count"], 2);
    assert_eq!(creators["census_page_count"], 2);
    assert_eq!(creators["format"], "creator_index_v1");
    assert_eq!(summary["sources"], creators["sources"]);
    assert_eq!(summary["sources"][0]["file"], "canonical.json");
    assert_eq!(summary["sources"][0]["size"], before.len());
    let nanos = fs::metadata(&source)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    assert_eq!(
        summary["sources"][0]["mtime_ns"].as_u64().unwrap() as u128,
        nanos
    );
    assert_eq!(creators["creators"].as_array().unwrap().len(), 1);
    assert_eq!(creators["creators"][0]["key"], "history:7");
    assert_eq!(creators["creators"][0]["total"], 2);
    assert!(summary["reports"].get("creators").is_none());
    assert_eq!(summary["reports"]["numeric"]["age_days"]["known"], 1);
    assert_eq!(
        summary["reports"]["numeric"]["age_days"]["groups"][0]["median"],
        1.0
    );
    assert_eq!(
        summary["reports"]["numeric"]["age_days"]["groups"][1]["missing"],
        1
    );
    assert_eq!(
        summary["reports"]["numeric"]["creator_prior_edits_main"]["groups"][0]["median"],
        3.0
    );
    assert_eq!(
        summary["reports"]["profiles"]["topics"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        summary["reports"]["profiles"]["features"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    assert_eq!(
        summary["reports"]["profiles"]["thresholds"]["vec"][2]
            .as_f64()
            .unwrap()
            .to_bits(),
        0.9859704508633368f64.to_bits()
    );
    assert_eq!(
        summary["article_preview"][0]["quality_score"]
            .as_f64()
            .unwrap()
            .to_bits(),
        0.9859704508633369f64.to_bits()
    );
    assert_eq!(fs::read(&source).unwrap(), before);
    let mut current = metadata.clone();
    current["creators"]["status"] = json!("collected");
    w.canonical(&current, &pages, None);
    let folder = w.0.join("current");
    let names = prepare_dashboard_exports(&source, &folder, None, None).unwrap();
    let creators = read_json(folder.join(names["creator_index_file"].as_str().unwrap()));
    assert_eq!(creators["creators"].as_array().unwrap().len(), 2);
    assert!(creators["creators"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["key"].as_str().unwrap().starts_with("current:")));
}

#[test]
fn preview_uses_full_unicode_casefold_title_id_ties_and_only_the_first_25_orphans() {
    let w = Workspace::new();
    let titles = [
        "ß", "SS", "ss", "ẞ", "ﬃoo", "ffiOo", "ffi", "ﬀ", "FF", "İ", "I\u{307}", "Σ", "σ", "ς",
        "ǰ", "J\u{30c}", "ᾈ", "K", "K", "Ω", "ω", "Ա", "ա", "А", "а", "A", "a", "Å", "å", "Z", "z",
        "Ꭰ", "ꭰ", "I", "i", "ı", "µ", "μ",
    ];
    let pages=titles.iter().enumerate().rev().map(|(i,title)|json!({"page_id":i+1,"title":title,"orphan":true,"category":format!("Category {i}"),"language":{"discard":"incidental"}})).collect::<Vec<_>>();
    let source = w.source(&json!(pages), None);
    let folder = w.0.join("prepared");
    let names = prepare_dashboard_exports(&source, &folder, None, None).unwrap();
    let summary = read_json(folder.join(names["summary_file"].as_str().unwrap()));
    let preview = summary["article_preview"].as_array().unwrap();
    let expected = [
        "A", "a", "FF", "ﬀ", "ffi", "ffiOo", "ﬃoo", "I", "i", "I\u{307}", "İ", "J\u{30c}", "ǰ",
        "K", "K", "SS", "ss", "ß", "ẞ", "Z", "z", "Å", "å", "ı", "µ",
    ];
    assert_eq!(
        preview
            .iter()
            .map(|row| row["title"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(preview.iter().all(|row| row.get("language").is_none()
        && row.as_object().unwrap().len() == OBSERVATION_COLUMNS.len()));
    assert_eq!(
        summary["reports"]["categories"].as_array().unwrap().len(),
        20
    );
    assert_eq!(summary["reports"]["overview"]["total"], titles.len());
    let source = w.source(
        &json!([
            {"page_id":3,"title":"Same","orphan":true},
            {"page_id":1,"title":"Same","orphan":true},
            {"page_id":2,"title":"Before","orphan":false}
        ]),
        None,
    );
    let folder = w.0.join("ties");
    let names = prepare_dashboard_exports(&source, &folder, None, None).unwrap();
    let summary = read_json(folder.join(names["summary_file"].as_str().unwrap()));
    assert_eq!(
        summary["article_preview"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["page_id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
}

#[test]
fn late_summary_validation_removes_prepared_editorial_and_article_products() {
    let w = Workspace::new();
    for invalid in [
        json!({"page_id":2,"title":null,"orphan":true}),
        json!({"page_id":2,"title":"","orphan":true}),
        json!({"page_id":2,"title":"B","orphan":true,"quality_score":{}}),
        json!({"page_id":2,"title":"B","orphan":true,"topic_labels":true}),
    ] {
        let source = w.source(
            &json!([{"page_id":1,"title":"A","orphan":true},invalid]),
            None,
        );
        let before = fs::read(&source).unwrap();
        let folder = w.0.join("prepared");
        assert!(prepare_dashboard_exports(&source, &folder, None, None).is_err());
        assert!(!folder.exists());
        assert_eq!(fs::read(&source).unwrap(), before);
        w.assert_files(&["canonical.json"]);
    }
}

#[cfg(unix)]
#[test]
fn failed_index_writes_remove_only_the_owned_preparation_directory() {
    let w = Workspace::new();
    let source = w.source(&json!([{"page_id":1,"title":"A","orphan":true}]), None);
    let long_source = w.0.join(format!("{}.json", "x".repeat(245)));
    fs::rename(&source, &long_source).unwrap();
    let before = fs::read(&long_source).unwrap();
    let folder = w.0.join("prepared");
    assert!(prepare_dashboard_exports(&long_source, &folder, None, None).is_err());
    assert!(!folder.exists());
    assert_eq!(fs::read(&long_source).unwrap(), before);
    assert_eq!(fs::read_dir(&w.0).unwrap().count(), 1);
}

#[test]
fn aggregate_indices_and_summaries_retain_member_denominators_intervals_and_scoped_creators() {
    let w = Workspace::new();
    let mut members = Vec::new();
    for language in ["vec", "lmo"] {
        let metadata = json!({"language":language,"complete":true,"demo":true,"started_at":if language=="vec" {"2026-01-01T01:00:00+01:00"}else{"2026-01-02T00:00:00Z"},"finished_at":if language=="vec" {"2026-01-03T00:00:00Z"}else{"2026-01-02T01:00:00+00:00"},"age_rule":if language=="vec" {"page_creation_event"}else{"mediawiki_history_page_creation_v2"},"creators":{"status":"not_collected"},"history":{"status":if language=="vec" {"collected"}else{"not_collected"},"matched_pages":1},"measures_rule":"creation_quality_creators_v3"});
        let file = format!("{language}.json");
        let path = w.0.join(&file);
        let pages = json!([{"page_id":1,"title":"Same","orphan":true,"history_creator_key":"same","history_creator_status":"observed_surviving_history","history_data_status":"matched","age_days":0.0,"length_bytes":0,"quality_score":0.9859704508633369},{"page_id":2,"title":"ß","orphan":true,"history_creator_key":"same","history_creator_status":"observed_surviving_history","history_data_status":"matched","age_days":null,"length_bytes":null}]);
        fs::write(
            &path,
            format!("{{\"metadata\":{metadata},\"pages\":{pages}}}"),
        )
        .unwrap();
        members.push(
            json!({"language":language,"file":file,"path":path,"page_count":2,"metadata":metadata}),
        );
    }
    let index = aggregate_index(&members).unwrap();
    assert_eq!(index["metadata"]["languages"], json!(["lmo", "vec"]));
    assert_eq!(index["metadata"]["age_rule"], "mixed_per_language");
    assert!(index["metadata"]["historical_age_rule"].is_null());
    assert_eq!(index["metadata"]["started_at"], "2026-01-01T01:00:00+01:00");
    assert_eq!(index["metadata"]["finished_at"], "2026-01-03T00:00:00Z");
    assert!(index["members"][0].get("path").is_none());
    members.sort_by(|a, b| a["language"].as_str().cmp(&b["language"].as_str()));
    let output = w.0.join("all-summary.json");
    write_aggregate_summary(&members, &output, &index["metadata"], true).unwrap();
    let summary = read_json(&output);
    let creators = read_json(w.0.join("all-creators.json"));
    assert_eq!(summary["census_page_count"], 4);
    assert_eq!(summary["reports"]["overview"]["total"], 4);
    assert_eq!(
        summary["metadata"]["history"],
        json!({"status":"partially_collected","matched_pages":1})
    );
    assert_eq!(summary["metadata"]["creators"]["status"], "not_collected");
    assert_eq!(summary["article_preview"][0]["title"], "Same");
    assert_eq!(summary["article_preview"][0]["language"], "lmo");
    assert_eq!(summary["article_preview"][1]["language"], "vec");
    assert_eq!(summary["sources"], creators["sources"]);
    assert!(summary["sources"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| !s["file"].as_str().unwrap().contains('/')));
    let keys = creators["creators"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["key"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(keys.contains(&"lmo:same"));
    assert!(keys.contains(&"vec:same"));
    let approved = fs::read(&output).unwrap();
    let approved_creators = fs::read(w.0.join("all-creators.json")).unwrap();
    members[1]["page_count"] = json!(3);
    assert!(write_aggregate_summary(&members, &output, &index["metadata"], true).is_err());
    assert_eq!(fs::read(&output).unwrap(), approved);
    assert_eq!(
        fs::read(w.0.join("all-creators.json")).unwrap(),
        approved_creators
    );
    assert!(write_aggregate_summary(
        &members,
        &w.0.join("invalid.json"),
        &index["metadata"],
        true
    )
    .is_err());
    members[1]["page_count"] = json!(2);
    members[1]["metadata"]["demo"] = json!(false);
    assert!(aggregate_index(&members).is_err());
    assert!(aggregate_index(&[]).is_err());
    assert!(aggregate_index(&[members[0].clone(), members[0].clone()]).is_err());
}

#[test]
fn comparison_names_keep_utf8_float_identity_and_every_reference_source() {
    let mut metadata = json!({"language":"vec","complete":true,"sql_release":"20261001","input_sha256":"abc","history":{"note":"È 🌙 ß","value":0.9859704508633369},"measures_rule":"creation_quality_creators_v3"});
    let languages = vec!["vec".into(), "lmo".into()];
    assert_eq!(
        comparison_filename("vec", &metadata, &languages).unwrap(),
        "vec-20261001-7c4ed1ac784d.json"
    );
    metadata["comparison"] = json!({"collections":[{"language":"lmo","input_sha256":"old"}]});
    let first = comparison_filename("vec", &metadata, &languages).unwrap();
    metadata["comparison"]["collections"][0]["input_sha256"] = json!("new");
    assert_ne!(
        first,
        comparison_filename("vec", &metadata, &languages).unwrap()
    );
    metadata["sql_release"] = Value::Null;
    metadata["demo"] = json!(true);
    assert_eq!(
        comparison_filename("vec", &metadata, &languages).unwrap(),
        "vec-demo.json"
    );
}

#[test]
fn available_aggregates_label_partial_comparison_reuse_verified_summaries_and_repair_stale_members()
{
    let w = Workspace::new();
    let mut manifest = Vec::new();
    for language in ["vec", "lmo"] {
        let metadata = json!({"language":language,"database":format!("{language}wiki"),"source":format!("Wikimedia SQL dumps / {language}wiki"),"complete":true,"sql_release":"20261001","dump_manifest_sha256":"a".repeat(64),"started_at":"2026-10-01T00:00:00Z","finished_at":"2026-10-02T00:00:00Z","measures_rule":"creation_quality_creators_v3"});
        let source = w.0.join(format!("{language}.json"));
        fs::write(&source,format!("{{\"metadata\":{metadata},\"pages\":[{{\"page_id\":1,\"title\":\"Same\",\"orphan\":true}},{{\"page_id\":2,\"title\":\"ß\",\"orphan\":false}}]}}")).unwrap();
        let prepared = w.0.join(format!("prepare-{language}"));
        let names = prepare_dashboard_exports(&source, &prepared, None, None).unwrap();
        for name in names.as_object().unwrap().values() {
            let name = name.as_str().unwrap();
            fs::rename(prepared.join(name), w.0.join(name)).unwrap();
        }
        fs::remove_dir(prepared).unwrap();
        let mut entry = json!({"file":format!("{language}.json"),"language":language,"kind":"wiki","date":metadata["finished_at"]});
        entry
            .as_object_mut()
            .unwrap()
            .extend(names.as_object().unwrap().clone());
        manifest.push(entry);
    }
    fs::write(w.0.join("manifest.json"), json!(manifest).to_string()).unwrap();
    let before = snapshot(&w.0);
    let index = publish_available_aggregate(&w.0).unwrap().unwrap();
    assert_eq!(index["metadata"]["comparison_coverage"], "partial");
    let first = read_json(w.0.join("manifest.json"));
    assert_eq!(first.as_array().unwrap().len(), 3);
    let aggregate = first.as_array().unwrap().last().unwrap();
    let summary_name = aggregate["summary_file"].clone();
    let creators_name = aggregate["creator_index_file"].clone();
    let summary = read_json(w.0.join(summary_name.as_str().unwrap()));
    assert_eq!(summary["census_page_count"], 4);
    assert_eq!(summary["reports"]["overview"]["orphans"], 2);
    assert_eq!(summary["metadata"]["comparison_coverage"], "partial");
    for (path, bytes) in &before {
        if path != "manifest.json" {
            assert_eq!(fs::read(w.0.join(path)).unwrap(), *bytes);
        }
    }
    publish_available_aggregate(&w.0).unwrap();
    let reused = read_json(w.0.join("manifest.json"));
    assert_eq!(
        reused.as_array().unwrap().last().unwrap()["summary_file"],
        summary_name
    );
    assert_eq!(
        reused.as_array().unwrap().last().unwrap()["creator_index_file"],
        creators_name
    );
    let old_summary = first[0]["summary_file"].as_str().unwrap();
    let mut stale = read_json(w.0.join(old_summary));
    stale["sources"][0]["size"] = json!(0);
    fs::write(w.0.join(old_summary), stale.to_string()).unwrap();
    publish_available_aggregate(&w.0).unwrap();
    let repaired = read_json(w.0.join("manifest.json"));
    assert_ne!(repaired[0]["summary_file"], first[0]["summary_file"]);
    assert_eq!(
        repaired.as_array().unwrap().last().unwrap()["summary_file"],
        summary_name
    );
    let repaired_summary = read_json(w.0.join(repaired[0]["summary_file"].as_str().unwrap()));
    assert!(repaired_summary["sources"][0]["size"].as_u64().unwrap() > 0);
    assert!(w.0.join(old_summary).is_file());
    let approved = fs::read(w.0.join("manifest.json")).unwrap();
    let source = w.0.join("vec.json");
    let mut bad = read_json(&source);
    bad["metadata"]["source"] = json!("Unapproved enrichment");
    fs::write(source, bad.to_string()).unwrap();
    assert!(publish_available_aggregate(&w.0).is_err());
    assert_eq!(fs::read(w.0.join("manifest.json")).unwrap(), approved);
}

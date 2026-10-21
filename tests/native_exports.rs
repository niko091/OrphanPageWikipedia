#![cfg(feature = "cli")]
use _native::dumps::{
    export_csv, prepare_browser_indices, ARTICLE_INDEX_COLUMNS, CSV_COLUMNS, OBSERVATION_COLUMNS,
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
        let path = self.0.join("canonical.json");
        let mut text =
            format!("{{\"metadata\":{{\"language\":\"vec\",\"complete\":true}},\"pages\":{pages}");
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
        assert!(prepare_browser_indices(&source, &prepared, None, None).is_err());
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
    let names = prepare_browser_indices(&source, &folder, None, None).unwrap();
    assert_eq!(names.as_object().unwrap().len(), 3);
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
    let old = prepare_browser_indices(&source, &first, None, None).unwrap();
    let frozen = snapshot(&first);
    let same = w.0.join("same");
    assert_eq!(
        prepare_browser_indices(&source, &same, None, None).unwrap(),
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
    let changed = prepare_browser_indices(&source, &next, None, None).unwrap();
    for key in ["observations_file", "articles_file", "profile_topics_dir"] {
        assert_ne!(old[key], changed[key]);
    }
    assert_eq!(snapshot(&first), frozen);
    let explicit = w.0.join("explicit");
    let names = prepare_browser_indices(
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
        assert!(prepare_browser_indices(&source, &prepared, Some(&expected), None).is_err());
        assert!(!prepared.exists());
    }
    for generation in [
        "",
        "a",
        "ABCDEF0123456789abcdef0123456789ab",
        "../0123456789abcdef0123456789abcd",
        "g123456789abcdef0123456789abcdef",
    ] {
        assert!(prepare_browser_indices(&source, &prepared, None, Some(generation)).is_err());
        assert!(!prepared.exists());
    }
    fs::create_dir(&prepared).unwrap();
    fs::write(prepared.join("approved"), "old").unwrap();
    assert!(prepare_browser_indices(&source, &prepared, None, None).is_err());
    assert_eq!(
        fs::read_to_string(prepared.join("approved")).unwrap(),
        "old"
    );
    assert!(prepare_browser_indices(&source, &source, None, None).is_err());
    assert!(prepare_browser_indices(&source, &w.0, None, None).is_err());
    fs::create_dir(w.0.join("empty")).unwrap();
    assert!(prepare_browser_indices(&source, &w.0.join("empty"), None, None).is_err());
    assert!(w.0.join("empty").is_dir());
    let accepted = w.0.join("accepted");
    assert!(prepare_browser_indices(
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
    let names = prepare_browser_indices(&source, &folder, None, None).unwrap();
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
    assert!(prepare_browser_indices(&long_source, &folder, None, None).is_err());
    assert!(!folder.exists());
    assert_eq!(fs::read(&long_source).unwrap(), before);
    assert_eq!(fs::read_dir(&w.0).unwrap().count(), 1);
}

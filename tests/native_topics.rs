#![cfg(feature = "cli")]

use _native::dumps::{
    predict_imported_topics, predict_topics, NativeTopicModel, TopicModel, TopicPrediction,
    SQL_TABLES,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha512};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orphanwiki-topics-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("config")).unwrap();
        fs::write(
            path.join("config/languages.json"),
            r#"{"version":1,"languages":["vec","lmo"]}"#,
        )
        .unwrap();
        Self(path)
    }
    fn model(&self, dim: i32) -> Value {
        // A deterministic, untrained test artifact in upstream's version-12
        // binary format. Nothing here represents research weights or data.
        let mut spec: Value = serde_json::from_str(include_str!("../config/topics.json")).unwrap();
        let mut bytes = Vec::new();
        for number in [793712314i32, 12, dim, 5, 1, 1, 5, 1, 4, 3, 0, 0, 0, 100] {
            bytes.extend(number.to_ne_bytes());
        }
        bytes.extend(0.0001f64.to_ne_bytes());
        for number in [68i32, 4, 64] {
            bytes.extend(number.to_ne_bytes());
        }
        bytes.extend(100i64.to_ne_bytes());
        bytes.extend((-1i64).to_ne_bytes());
        let words = ["Q1".to_owned(), "Q2".into(), "Q3".into(), "</s>".into()];
        let labels = spec["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| format!("__label__{}", v.as_str().unwrap().replace(' ', "_")))
            .collect::<Vec<_>>();
        for (index, word) in words.iter().chain(&labels).enumerate() {
            bytes.extend(word.as_bytes());
            bytes.push(0);
            bytes.extend(10i64.to_ne_bytes());
            bytes.push(u8::from(index >= 4));
        }
        bytes.push(0);
        bytes.extend(4i64.to_ne_bytes());
        bytes.extend(i64::from(dim).to_ne_bytes());
        for _ in 0..4 {
            for column in 0..dim {
                bytes.extend(if column == 0 { 1f32 } else { 0f32 }.to_ne_bytes());
            }
        }
        bytes.push(0);
        bytes.extend(64i64.to_ne_bytes());
        bytes.extend(i64::from(dim).to_ne_bytes());
        for index in 0..64 {
            for column in 0..dim {
                let value = if column != 0 {
                    0f32
                } else {
                    match index {
                        0 => -0.02,
                        1 => 0.0,
                        2 => 0.02,
                        _ => -1.0,
                    }
                };
                bytes.extend(value.to_ne_bytes());
            }
        }
        fs::write(self.0.join("model.bin"), &bytes).unwrap();
        spec["model_sha512"] = json!(format!("{:x}", Sha512::digest(bytes)));
        fs::write(self.0.join("config/topics.json"), spec.to_string()).unwrap();
        spec
    }
    fn sql(&self) -> Value {
        let files = SQL_TABLES
            .iter()
            .map(|&table| (table.to_owned(), json!({"sha256_local":table})))
            .collect::<serde_json::Map<_, _>>();
        let manifest =
            json!({"language":"vec","database":"vecwiki","sql_release":"20261001","files":files});
        fs::write(self.0.join("manifest.json"), manifest.to_string()).unwrap();
        let metadata = json!({"language":"vec","database":"vecwiki","source":"Wikimedia SQL dumps / vecwiki","complete":true,"demo":false,"sql_release":"20261001","finished_at":"2026-10-02T00:00:00+00:00",
            "item_mapping":{"source":"page_props.wikibase_item","complete":true},"dump_manifest":self.0.join("manifest.json"),"dump_manifest_sha256":format!("{:x}",Sha256::digest(manifest.to_string()))});
        let source = Connection::open(self.0.join("vecwiki-20261001.sqlite")).unwrap();
        source.execute_batch("CREATE TABLE imported(table_name TEXT PRIMARY KEY,sha256 TEXT);CREATE TABLE dump_page(id INTEGER,ns INTEGER,redirect INTEGER,title TEXT);CREATE TABLE dump_redirect(page_id INTEGER,ns INTEGER,title TEXT,interwiki TEXT);CREATE TABLE dump_category(source_id INTEGER,target_id INTEGER,title TEXT);CREATE TABLE dump_target(id INTEGER,ns INTEGER,title TEXT);CREATE TABLE dump_prop(page_id INTEGER,name TEXT);CREATE TABLE pages(id INTEGER PRIMARY KEY,data TEXT);CREATE TABLE links(id INTEGER,target TEXT);CREATE INDEX links_page ON links(id);CREATE TABLE categories(id INTEGER,target TEXT);CREATE INDEX categories_page ON categories(id);").unwrap();
        for table in SQL_TABLES {
            source
                .execute("INSERT INTO imported VALUES(?,?)", [table, table])
                .unwrap();
        }
        let mut pages = Vec::new();
        for id in 1..=521 {
            let title = match id {
                1 => "Source".into(),
                2 => "Target".into(),
                3 => "Outside".into(),
                _ => format!("P{id}"),
            };
            let item = match id {
                1 => "Q1".into(),
                2 => "Q2".into(),
                3 => "Q99999".into(),
                4 => "Q1".into(),
                5 => "Q0".into(),
                _ => format!("Q{id}"),
            };
            let mut page =
                json!({"page_id":id,"title":title,"wikibase_item":item,"revision_id":id+1000});
            source
                .execute("INSERT INTO dump_page VALUES(?,0,0,?)", params![id, title])
                .unwrap();
            source
                .execute(
                    "INSERT INTO pages VALUES(?,?)",
                    params![id, page.to_string()],
                )
                .unwrap();
            let targets = match id {
                1 => vec!["Source", "Alias", "Target", "Alias2", "Missing", "Cycle"],
                2 | 3 | 521 => vec![],
                4 => vec!["Outside"],
                _ => vec!["Target"],
            };
            for target in &targets {
                source
                    .execute("INSERT INTO links VALUES(?,?)", params![id, target])
                    .unwrap();
            }
            page["links"] = json!(targets);
            page["categories"] = json!([]);
            pages.push(page);
        }
        let redirects =
            json!({"Alias":"Alias2","Alias2":"Target","Cycle":"Cycle2","Cycle2":"Cycle"});
        for (i, (alias, target)) in redirects.as_object().unwrap().iter().enumerate() {
            source
                .execute(
                    "INSERT INTO dump_page VALUES(?,0,1,?)",
                    params![10000 + i, alias],
                )
                .unwrap();
            source
                .execute(
                    "INSERT INTO dump_redirect VALUES(?,0,?,'')",
                    params![10000 + i, target.as_str()],
                )
                .unwrap();
        }
        let raw =
            json!({"metadata":metadata,"pages":pages,"redirects":redirects,"category_parents":{}});
        // Imported metadata must be the first section of a streaming graph.
        fs::write(
            self.0.join("vec-raw.json"),
            format!(
                "{{\"metadata\":{},\"pages\":{},\"redirects\":{},\"category_parents\":{{}}}}",
                raw["metadata"], raw["pages"], raw["redirects"]
            ),
        )
        .unwrap();
        raw
    }
    fn infer(
        &self,
        model: &mut impl TopicModel,
        spec: &Value,
        size: usize,
        deadline: Option<Instant>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        predict_imported_topics(
            &self.0.join("vec-raw.json"),
            &self.0.join("result.json"),
            model,
            spec,
            size,
            deadline,
            Some(&self.0),
        )
    }
    fn assert_clean(&self) {
        assert!(fs::read_dir(&self.0).unwrap().all(|p| !p
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')));
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

struct MockModel {
    labels: Vec<String>,
    calls: Vec<Vec<String>>,
    score: f64,
    delay: bool,
    malformed: usize,
    change_input: Option<PathBuf>,
}
impl MockModel {
    fn new(spec: &Value) -> Self {
        Self {
            labels: spec["labels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| format!("__label__{}", v.as_str().unwrap().replace(' ', "_")))
                .collect(),
            calls: Vec::new(),
            score: 0.5,
            delay: false,
            malformed: 0,
            change_input: None,
        }
    }
}
impl TopicModel for MockModel {
    fn labels(&self) -> Result<Vec<String>, String> {
        Ok(self.labels.clone())
    }
    fn word_ids(&mut self, words: &[&str]) -> Result<Vec<i32>, String> {
        Ok(words
            .iter()
            .map(|word| if *word == "Q99999" { -1 } else { 1 })
            .collect())
    }
    fn predict(&mut self, inputs: &[String]) -> Result<Vec<TopicPrediction>, String> {
        self.calls.push(inputs.to_vec());
        if let Some(path) = self.change_input.take() {
            use std::io::Write;
            fs::OpenOptions::new()
                .append(true)
                .open(path)
                .unwrap()
                .write_all(b" ")
                .unwrap();
        }
        if self.delay {
            std::thread::sleep(Duration::from_millis(30));
        }
        if self.malformed == 1 {
            return Ok(Vec::new());
        }
        if self.malformed == 2 {
            return Err("Synthetic native failure".into());
        }
        if self.malformed == 3 && self.calls.len() > 1 {
            return Err("Synthetic failure after completed batches".into());
        }
        Ok(inputs
            .iter()
            .map(|_| TopicPrediction {
                labels: self.labels.clone(),
                probabilities: vec![self.score; 64],
            })
            .collect())
    }
}
fn spec() -> Value {
    serde_json::from_str(include_str!("../config/topics.json")).unwrap()
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn real_fasttext_engine_reads_verified_models_and_retains_all_float_probabilities() {
    for dim in [16, 17, 32, 64] {
        let work = Workspace::new();
        let spec = work.model(dim);
        let mut model = NativeTopicModel::load(&work.0.join("model.bin"), &spec).unwrap();
        assert_eq!(model.labels().unwrap().len(), 64);
        assert_eq!(model.word_ids(&["Q2", "Q999"]).unwrap(), [1, -1]);
        assert!(model.word_ids(&["Q2\0x"]).is_err());
        let words = vec!["Q2"; 4097];
        assert!(model.word_ids(&words).unwrap().iter().all(|id| *id == 1));
        let bulk = model.predict(&vec!["Q2 ".repeat(512); 257]).unwrap();
        assert_eq!(bulk.len(), 257);
        assert!(bulk
            .iter()
            .all(|p| p.labels == bulk[0].labels && p.probabilities == bulk[0].probabilities));
        let response = model.predict(&["Q2".into(), "Q2 Q3".into()]).unwrap();
        assert_eq!(response.len(), 2);
        for row in &response {
            assert_eq!(row.labels.len(), 64);
            assert_eq!(row.probabilities.len(), 64);
            assert!(row
                .probabilities
                .iter()
                .all(|&p| p.is_finite() && (0.0..=1.0001).contains(&p)));
            let scores = row
                .labels
                .iter()
                .zip(&row.probabilities)
                .map(|(l, p)| (l.as_str(), *p))
                .collect::<std::collections::HashMap<_, _>>();
            let label = |i: usize| {
                format!(
                    "__label__{}",
                    spec["labels"][i].as_str().unwrap().replace(' ', "_")
                )
            };
            assert!(scores[&label(0).as_str()] < 0.5);
            assert!(scores[&label(1).as_str()] > 0.5); // upstream log(x + 1e-5)
            assert_eq!(scores[&label(1).as_str()], 0.5000100135803223);
            assert!(scores[&label(2).as_str()] >= scores[&label(1).as_str()]);
        }
        assert_eq!(response[0].labels, response[1].labels);
        assert_eq!(response[0].probabilities, response[1].probabilities);
        assert!(model.predict(&["Q2\nQ3".into()]).is_err());
        assert!(model.predict(&["Q2\0Q3".into()]).is_err());
    }
}

#[test]
fn bad_model_checksum_configuration_taxonomy_and_format_fail_before_prediction() {
    let work = Workspace::new();
    let spec = work.model(16);
    for change in [
        json!({"threshold":0.6}),
        json!({"labels":["bad"]}),
        json!({"version":2}),
        json!({"model_sha512":"a".repeat(128)}),
    ] {
        let mut bad = spec.clone();
        for (key, value) in change.as_object().unwrap() {
            bad[key] = value.clone();
        }
        assert!(NativeTopicModel::load(&work.0.join("model.bin"), &bad).is_err());
    }
    let mut bad = spec.clone();
    bad["labels"][0] = json!("Unknown.Label");
    assert!(NativeTopicModel::load(&work.0.join("model.bin"), &bad).is_err());
    fs::write(work.0.join("model.bin"), b"invalid model").unwrap();
    bad = spec;
    bad["model_sha512"] = json!(format!("{:x}", Sha512::digest(b"invalid model")));
    assert!(NativeTopicModel::load(&work.0.join("model.bin"), &bad).is_err());
}

#[test]
fn sql_batches_preserve_the_entire_census_inputs_missing_and_strict_threshold() {
    let work = Workspace::new();
    let mut raw = work.sql();
    let spec = spec();
    let mut expected = None;
    for size in [1, 7, 256, 10000] {
        let mut model = MockModel::new(&spec);
        let metadata = work.infer(&mut model, &spec, size, None).unwrap();
        let output = read(&work.0.join("result.json"));
        assert_eq!(output["pages"].as_array().unwrap().len(), 521);
        assert_eq!(metadata["predicted_pages"], 517);
        assert_eq!(metadata["missing_pages"], 4);
        assert_eq!(output["pages"][0]["topic_input_count"], 1);
        assert_eq!(output["pages"][0]["topic_labels"], json!([]));
        assert_eq!(output["pages"][1]["topic_status"], "no_mapped_outlinks");
        assert_eq!(output["pages"][3]["topic_status"], "no_model_vocabulary");
        assert!(output["pages"][3]["topic_labels"].is_null());
        assert_eq!(output["pages"][520]["topic_status"], "no_mapped_outlinks");
        assert!(model
            .calls
            .iter()
            .all(|batch| batch.len() <= size && batch.iter().all(|text| text == "Q2")));
        if let Some((ref pages, ref checksum)) = expected {
            assert_eq!(&output["pages"], pages);
            assert_eq!(&metadata["input_sha256"], checksum);
        } else {
            expected = Some((output["pages"].clone(), metadata["input_sha256"].clone()));
        }
    }
    raw["metadata"]["demo"] = json!(true);
    let memory = predict_topics(&raw, &mut MockModel::new(&spec), &spec, 7, None).unwrap();
    assert_eq!(memory["pages"], expected.unwrap().0);
    for score in [
        f64::from_bits(0.5f64.to_bits() - 1),
        0.5,
        f64::from_bits(0.5f64.to_bits() + 1),
    ] {
        let mut model = MockModel::new(&spec);
        model.score = score;
        let result = predict_topics(&raw, &mut model, &spec, 7, None).unwrap();
        assert_eq!(
            result["pages"][0]["topic_labels"].as_array().unwrap().len(),
            if score > 0.5 { 64 } else { 0 }
        );
    }
    work.assert_clean();
}

#[test]
fn timeouts_and_bad_native_responses_keep_the_approved_export_and_clean_spools() {
    let work = Workspace::new();
    work.sql();
    let spec = spec();
    fs::write(work.0.join("result.json"), b"approved prior export").unwrap();
    let mut cases = Vec::new();
    for score in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
        let mut model = MockModel::new(&spec);
        model.score = score;
        cases.push(model);
    }
    for malformed in [1, 2, 3] {
        let mut model = MockModel::new(&spec);
        model.malformed = malformed;
        cases.push(model);
    }
    let mut duplicate = MockModel::new(&spec);
    duplicate.labels[1] = duplicate.labels[0].clone();
    cases.push(duplicate);
    for mut model in cases {
        assert!(work.infer(&mut model, &spec, 7, None).is_err());
        assert_eq!(
            fs::read(work.0.join("result.json")).unwrap(),
            b"approved prior export"
        );
        work.assert_clean();
    }
    let mut model = MockModel::new(&spec);
    model.delay = true;
    assert!(work
        .infer(
            &mut model,
            &spec,
            7,
            Some(Instant::now() + Duration::from_millis(15))
        )
        .is_err());
    assert!(work
        .infer(&mut model, &spec, 7, Some(Instant::now()))
        .is_err());
    assert_eq!(
        fs::read(work.0.join("result.json")).unwrap(),
        b"approved prior export"
    );
    let mut changed = MockModel::new(&spec);
    changed.change_input = Some(work.0.join("vec-raw.json"));
    assert!(work.infer(&mut changed, &spec, 7, None).is_err());
    assert_eq!(
        fs::read(work.0.join("result.json")).unwrap(),
        b"approved prior export"
    );
    work.assert_clean();
}

#[test]
fn altered_staging_unapproved_graphs_and_overlapping_outputs_are_rejected() {
    for operation in 0..11 {
        let work = Workspace::new();
        work.sql();
        let spec = spec();
        let source = Connection::open(work.0.join("vecwiki-20261001.sqlite")).unwrap();
        match operation {
            0 => {
                source
                    .execute("DELETE FROM imported WHERE table_name='page'", [])
                    .unwrap();
            }
            1 => {
                source.execute("DELETE FROM pages WHERE id=2", []).unwrap();
            }
            2 => {
                source
                    .execute("INSERT INTO links VALUES(99999,'Target')", [])
                    .unwrap();
            }
            3 => {
                source
                    .execute("INSERT INTO categories VALUES(-1,'Orphan')", [])
                    .unwrap();
            }
            4 => {
                let mut raw = read(&work.0.join("vec-raw.json"));
                raw["metadata"]["complete"] = json!(false);
                fs::write(
                    work.0.join("vec-raw.json"),
                    format!("{{\"metadata\":{}}}", raw["metadata"]),
                )
                .unwrap();
            }
            5 => {
                fs::write(work.0.join("manifest.json"), b"{}").unwrap();
            }
            6 => {
                source
                    .execute("UPDATE pages SET data='{}' WHERE id=3", [])
                    .unwrap();
            }
            7 => {
                source
                    .execute("INSERT INTO links VALUES(-1,'Target')", [])
                    .unwrap();
            }
            8 => source
                .execute_batch("DELETE FROM dump_page; DELETE FROM pages")
                .unwrap(),
            9 => {
                let mut manifest = read(&work.0.join("manifest.json"));
                manifest["files"]["page"]
                    .as_object_mut()
                    .unwrap()
                    .remove("sha256_local");
                fs::write(work.0.join("manifest.json"), manifest.to_string()).unwrap();
                source
                    .execute("DELETE FROM imported WHERE table_name='page'", [])
                    .unwrap();
                let mut raw = read(&work.0.join("vec-raw.json"));
                raw["metadata"]["dump_manifest_sha256"] =
                    json!(format!("{:x}", Sha256::digest(manifest.to_string())));
                fs::write(
                    work.0.join("vec-raw.json"),
                    format!("{{\"metadata\":{}}}", raw["metadata"]),
                )
                .unwrap();
            }
            _ => {
                let mut raw = read(&work.0.join("vec-raw.json"));
                raw["metadata"]
                    .as_object_mut()
                    .unwrap()
                    .remove("finished_at");
                fs::write(
                    work.0.join("vec-raw.json"),
                    format!("{{\"metadata\":{}}}", raw["metadata"]),
                )
                .unwrap();
            }
        }
        fs::write(work.0.join("result.json"), b"approved").unwrap();
        assert!(
            work.infer(&mut MockModel::new(&spec), &spec, 7, None)
                .is_err(),
            "case {operation}"
        );
        assert_eq!(fs::read(work.0.join("result.json")).unwrap(), b"approved");
        work.assert_clean();
    }
    let work = Workspace::new();
    work.sql();
    let spec = spec();
    for input in ["vec-raw.json", "manifest.json", "vecwiki-20261001.sqlite"] {
        let before = fs::read(work.0.join(input)).unwrap();
        assert!(predict_imported_topics(
            &work.0.join("vec-raw.json"),
            &work.0.join(input),
            &mut MockModel::new(&spec),
            &spec,
            7,
            None,
            Some(&work.0)
        )
        .is_err());
        assert_eq!(fs::read(work.0.join(input)).unwrap(), before);
    }
}

#[test]
fn standalone_cli_uses_one_verified_engine_and_defers_whole_editions() {
    let work = Workspace::new();
    let mut raw = work.sql();
    raw["metadata"]["dump_manifest"] = json!("manifest.json");
    fs::write(
        work.0.join("vec-raw.json"),
        format!(
            "{{\"metadata\":{},\"pages\":{},\"redirects\":{},\"category_parents\":{{}}}}",
            raw["metadata"], raw["pages"], raw["redirects"]
        ),
    )
    .unwrap();
    work.model(16);
    let run = |budget: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_orphanwiki"))
            .args([
                "--repo-root",
                work.0.to_str().unwrap(),
                "topics",
                "--languages",
                "vec",
                "--input-dir",
                ".",
                "--output-dir",
                "out",
                "--model",
                "model.bin",
                "--work-dir",
                ".",
                "--batch-size",
                "7",
                "--time-budget-seconds",
                budget,
            ])
            .env_clear()
            .current_dir(std::env::temp_dir())
            .output()
            .unwrap()
    };
    let output = run("60");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = read(&work.0.join("out/topic-run.json"));
    assert_eq!(report["completed_languages"], json!(["vec"]));
    assert_eq!(report["wikis"][0]["census_pages"], 521);
    assert!(!work.0.join("out/lmo-topics.json").exists());
    let before = fs::read(work.0.join("out/vec-topics.json")).unwrap();
    let output = run("0.000000001");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = read(&work.0.join("out/topic-run.json"));
    assert_eq!(report["deferred_languages"], json!(["vec"]));
    assert_eq!(report["completed_languages"], json!([]));
    assert_eq!(
        fs::read(work.0.join("out/vec-topics.json")).unwrap(),
        before
    );
    let mut lmo = read(&work.0.join("vec-raw.json"));
    lmo["metadata"]["language"] = json!("lmo");
    lmo["metadata"]["database"] = json!("lmowiki");
    lmo["metadata"]["source"] = json!("synthetic fixture");
    lmo["metadata"]["demo"] = json!(true);
    fs::write(
        work.0.join("lmo-raw.json"),
        format!(
            "{{\"metadata\":{},\"pages\":{},\"redirects\":{},\"category_parents\":{{}}}}",
            lmo["metadata"], lmo["pages"], lmo["redirects"]
        ),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_orphanwiki"))
        .args([
            "--repo-root",
            work.0.to_str().unwrap(),
            "topics",
            "--input-dir",
            ".",
            "--output-dir",
            "all",
            "--model",
            "model.bin",
            "--work-dir",
            ".",
            "--time-budget-seconds",
            "60",
        ])
        .env_clear()
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read(&work.0.join("all/topic-run.json"))["completed_languages"],
        json!(["vec", "lmo"])
    );
    assert_eq!(
        read(&work.0.join("all/lmo-topics.json"))["metadata"]["language"],
        "lmo"
    );
    work.assert_clean();
}

#[test]
fn synthetic_errors_and_empty_graphs_do_not_invent_predictions() {
    let work = Workspace::new();
    let mut raw = work.sql();
    raw["metadata"]["demo"] = json!(true);
    let spec = spec();
    for mutate in 0..4 {
        let mut bad = raw.clone();
        match mutate {
            0 => bad["metadata"]["demo"] = json!(false),
            1 => bad["metadata"]["complete"] = json!(false),
            2 => bad["metadata"]["item_mapping"]["complete"] = json!(false),
            _ => {
                bad["pages"][1]["page_id"] = json!(1);
            }
        }
        assert!(predict_topics(&bad, &mut MockModel::new(&spec), &spec, 7, None).is_err());
    }
    raw["pages"] = json!([]);
    let result = predict_topics(&raw, &mut MockModel::new(&spec), &spec, 7, None).unwrap();
    assert_eq!(result["pages"], json!([]));
    assert_eq!(result["metadata"]["predicted_pages"], 0);
    assert!(predict_topics(&raw, &mut MockModel::new(&spec), &spec, 0, None).is_err());
    assert!(predict_topics(
        &raw,
        &mut MockModel::new(&spec),
        &spec,
        7,
        Some(Instant::now())
    )
    .is_err());
}

#[test]
fn sql_without_eligible_inputs_keeps_every_page_without_calling_the_model() {
    let work = Workspace::new();
    work.sql();
    let spec = spec();
    let db = Connection::open(work.0.join("vecwiki-20261001.sqlite")).unwrap();
    db.execute("DELETE FROM links", []).unwrap();
    let mut model = MockModel::new(&spec);
    let metadata = work.infer(&mut model, &spec, 7, None).unwrap();
    assert!(model.calls.is_empty());
    assert_eq!(metadata["predicted_pages"], 0);
    assert_eq!(metadata["missing_pages"], 521);
    assert!(
        read(&work.0.join("result.json"))["pages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|page| page["topic_labels"].is_null()
                && page["topic_status"] == "no_mapped_outlinks")
    );
}

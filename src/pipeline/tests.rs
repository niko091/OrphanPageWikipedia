use super::benchmark::{
    benchmark_outputs, benchmark_summary, canonical_fingerprint, local_output, BenchmarkOutput,
};
use super::checkpoints::signature;
use super::workflow::{path_text, remove_downloads, workflow};
use super::*;

struct Fixture {
    root: PathBuf,
    options: PipelineOptions,
    steps: FakeSteps,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("orphanwiki-pipeline-{}", runtime::unique_id()));
        std::fs::create_dir_all(root.join("config")).unwrap();
        write(
            &root.join("config/languages.json"),
            &json!({"version":1,"languages":["vec","lmo"]}),
        );
        write(&root.join("config/categories.json"), &json!({}));
        write(
            &root.join("config/topics.json"),
            &serde_json::from_str(include_str!("../../config/topics.json")).unwrap(),
        );
        write(
            &root.join("config/quality.json"),
            &json!({"model":"synthetic controller fixture"}),
        );
        let options = PipelineOptions {
            root: root.clone(),
            languages_file: root.join("config/languages.json"),
            selected: vec![("vec".into(), "vecwiki".into())],
            sql_release: "20261001".into(),
            history_release: "2026-09".into(),
            content_release: "2026-10-01".into(),
            dump_root: root.join("dumps"),
            import_dir: root.join("imported"),
            output_dir: root.join("exports"),
            collection_database: None,
            work_dir: root.join("staging"),
            log_dir: root.join("logs"),
            model: root.join("unused-model.bin"),
            model_config: root.join("config/topics.json"),
            topic_budget_seconds: 86400.0,
            topic_batch_size: 256,
            history_cache_mib: 256,
            history_commit_rows: 250000,
            metrics_interval: 0.1,
            no_phase_spans: false,
            force: false,
            plan: false,
        };
        let steps = FakeSteps {
            root: root.clone(),
            calls: Vec::new(),
            logs: Vec::new(),
            fail: None,
            incomplete_topics: false,
            generation: 0,
        };
        Self {
            root,
            options,
            steps,
        }
    }
    fn run(&mut self) -> PipelineResult<()> {
        workflow(&self.options, &mut self.steps)
    }
    fn reset(&mut self) {
        self.steps.calls.clear();
        self.steps.logs.clear();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn write(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    if read_json(path).ok().as_ref() == Some(value) {
        return;
    }
    if value.get("metadata").is_some() {
        let mut file = File::create(path).unwrap();
        file.write_all(b"{").unwrap();
        let mut first = true;
        for key in ["metadata", "pages"].into_iter().chain(
            value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .filter(|s| !matches!(*s, "metadata" | "pages")),
        ) {
            let Some(value) = value.get(key) else {
                continue;
            };
            if !first {
                file.write_all(b",").unwrap();
            }
            first = false;
            serde_json::to_writer(&mut file, key).unwrap();
            file.write_all(b":").unwrap();
            serde_json::to_writer(&mut file, value).unwrap();
        }
        file.write_all(b"}\n").unwrap();
    } else {
        // Native operational documents contain trailing whitespace too.
        dumps::DumpClient::<dumps::WikimediaHttp>::write_json(path, value).unwrap();
    }
}

struct FakeSteps {
    root: PathBuf,
    calls: Vec<Vec<String>>,
    logs: Vec<String>,
    fail: Option<String>,
    incomplete_topics: bool,
    generation: usize,
}

impl FakeSteps {
    fn publish(&mut self, languages: &[String], folder: &Path, input: &Path) {
        self.generation += 1;
        std::fs::create_dir_all(folder).unwrap();
        let mut manifest = read_json(&folder.join("manifest.json"))
            .unwrap_or(json!([]))
            .as_array()
            .unwrap()
            .clone();
        let mut members = Vec::new();
        for language in languages {
            let graph = read_json(&input.join(format!("{language}-raw.json"))).unwrap();
            let mut metadata = graph["metadata"].clone();
            metadata["age_rule"] = json!(dumps::HISTORY_RULE);
            metadata["measures_rule"] = json!("creation_quality_creators_v3");
            metadata["category_rules"] =
                read_json(&self.root.join("config/categories.json")).unwrap();
            metadata["topics"] =
                dumps::first_metadata(&input.join(format!("{language}-topics.json"))).unwrap();
            metadata["topics"]["status"] = json!("predicted");
            metadata["history"] =
                dumps::first_metadata(&input.join(format!("{language}-history.json"))).unwrap();
            metadata["history"]["status"] = json!("collected");
            metadata["quality"] =
                dumps::first_metadata(&input.join(format!("{language}-quality.json"))).unwrap();
            if let Some(manifest) = metadata["dump_manifest"].as_str() {
                let path = Path::new(manifest)
                    .parent()
                    .unwrap()
                    .join("content-2026-10-01.json");
                metadata["quality"]["manifest_sha256"] = json!(dumps::file_sha256(&path).unwrap());
            }
            if languages.len() > 1 {
                metadata["comparison"] = json!({"status":"compared","reference_languages":languages.iter().filter(|l|*l!=language).collect::<Vec<_>>()});
            }
            let file = format!("{language}-result-{}.json", self.generation);
            write(
                &folder.join(&file),
                &json!({"metadata":metadata,"pages":[{"page_id":1,"title":"A","orphan":true},{"page_id":2,"title":"B","orphan":false}],
                "link_candidates":{"1":{"status":"compared","candidate_count":0,"candidates":[]}}}),
            );
            let private = folder.join(format!("private-{}", runtime::unique_id()));
            let names = dumps::prepare_dashboard_exports(&folder.join(&file), &private, None, None)
                .unwrap();
            for entry in std::fs::read_dir(&private).unwrap() {
                let entry = entry.unwrap();
                std::fs::rename(entry.path(), folder.join(entry.file_name())).unwrap();
            }
            std::fs::remove_dir(private).unwrap();
            let mut entry = names;
            for (key, value) in [
                ("file", json!(file)),
                ("language", json!(language)),
                ("date", metadata["finished_at"].clone()),
                ("kind", json!("wiki")),
                ("published_at", json!(format!("{:08}", self.generation))),
            ] {
                entry[key] = value;
            }
            manifest.push(entry);
            members.push(json!({"language":language,"file":file,"path":folder.join(&file),"page_count":2,"metadata":metadata}));
        }
        if languages.len() > 1 {
            members.sort_by(|a, b| a["language"].as_str().cmp(&b["language"].as_str()));
            let index = dumps::aggregate_index(&members).unwrap();
            let stem = format!("all-{}", self.generation);
            write(&folder.join(format!("{stem}.json")), &index);
            dumps::write_aggregate_summary(
                &members,
                &folder.join(format!("{stem}-summary.json")),
                &index["metadata"],
                true,
            )
            .unwrap();
            manifest.push(json!({"kind":"aggregate","language":"all","file":format!("{stem}.json"),"summary_file":format!("{stem}-summary.json"),"creator_index_file":format!("{stem}-creators.json")}));
        }
        write(&folder.join("manifest.json"), &json!(manifest));
    }
}

impl Steps for FakeSteps {
    fn log(&mut self, text: &str) -> PipelineResult<()> {
        self.logs.push(text.into());
        Ok(())
    }
    fn run(&mut self, _name: &str, args: &[String]) -> PipelineResult<()> {
        self.calls.push(args.to_vec());
        let command = args[0].as_str();
        if self.fail.as_deref() == Some(command) {
            return Err("Synthetic child failure".into());
        }
        if args.iter().any(|s| s == "--plan") {
            return Ok(());
        }
        let value = |key: &str| args[args.iter().position(|v| v == key).unwrap() + 1].clone();
        let languages = || {
            let start = args.iter().position(|v| v == "--languages").unwrap() + 1;
            args[start..]
                .iter()
                .take_while(|v| !v.starts_with("--"))
                .cloned()
                .collect::<Vec<_>>()
        };
        match command {
            "fetch-dumps" => {
                for language in languages() {
                    let database = format!("{language}wiki");
                    let root = PathBuf::from(value("--root")).join(&database);
                    let mut files = Map::new();
                    for name in dumps::SQL_TABLES.into_iter().chain(["history.tsv.bz2"]) {
                        let path = root.join(name);
                        std::fs::create_dir_all(&root).unwrap();
                        std::fs::write(&path, name).unwrap();
                        files.insert(name.into(),json!({"path":path,"sha256_local":dumps::file_sha256(&path).unwrap(),"bytes":name.len()}));
                    }
                    write(
                        &root.join("manifest-20261001-2026-09.json"),
                        &json!({"language":language,"database":database,"sql_release":"20261001","history_release":"2026-09","files":files}),
                    );
                }
            }
            "import-dumps" | "import-history" => {
                let manifest = PathBuf::from(value("--manifest"));
                let meta = read_json(&manifest).unwrap();
                let language = meta["language"].as_str().unwrap();
                let database = meta["database"].as_str().unwrap();
                let stage =
                    PathBuf::from(value("--work-dir")).join(format!("{database}-20261001.sqlite"));
                std::fs::create_dir_all(stage.parent().unwrap()).unwrap();
                let db = rusqlite::Connection::open(&stage).unwrap();
                db.execute_batch("CREATE TABLE IF NOT EXISTS imported(table_name TEXT PRIMARY KEY,sha256 TEXT);CREATE TABLE IF NOT EXISTS pages(id INTEGER PRIMARY KEY,data TEXT);CREATE TABLE IF NOT EXISTS history_imported(file TEXT,phase TEXT,sha256 TEXT,PRIMARY KEY(file,phase));").unwrap();
                if command == "import-dumps" {
                    for name in dumps::SQL_TABLES {
                        db.execute(
                            "INSERT OR REPLACE INTO imported VALUES (?,?)",
                            rusqlite::params![
                                name,
                                meta["files"][name]["sha256_local"].as_str().unwrap()
                            ],
                        )
                        .unwrap();
                    }
                    db.execute_batch("INSERT OR REPLACE INTO pages VALUES(1,'{}'),(2,'{}')")
                        .unwrap();
                    write(
                        Path::new(&value("--output")),
                        &json!({"metadata":{"language":language,"database":database,"source":format!("Wikimedia SQL dumps / {database}"),"complete":true,
                        "started_at":"2026-10-01T00:00:00Z","finished_at":"2026-10-02T00:00:00Z","sql_release":"20261001","dump_manifest":manifest,"dump_manifest_sha256":dumps::file_sha256(&manifest).unwrap()},
                        "pages":[{"page_id":1,"title":"A"},{"page_id":2,"title":"B"}]}),
                    );
                }
                let digest = meta["files"]["history.tsv.bz2"]["sha256_local"]
                    .as_str()
                    .unwrap();
                for (phase, suffix) in [
                    ("initial", dumps::HISTORY_RULE),
                    ("activity", "synthetic-user-set"),
                ] {
                    db.execute(
                        "INSERT OR REPLACE INTO history_imported VALUES (?,?,?)",
                        rusqlite::params!["history.tsv.bz2", phase, format!("{digest}:{suffix}")],
                    )
                    .unwrap();
                }
                let output = value(if command == "import-dumps" {
                    "--history-output"
                } else {
                    "--output"
                });
                write(
                    Path::new(&output),
                    &json!({"metadata":{"source":format!("MediaWiki History dumps / {database}"),"language":language,"database":database,"complete":true,"sql_release":"20261001","history_release":"2026-09",
                    "creation_rule":dumps::HISTORY_RULE,"history_files":{"history.tsv.bz2":digest}},"pages":[]}),
                );
            }
            "topics" => {
                for language in languages() {
                    let graph =
                        PathBuf::from(value("--input-dir")).join(format!("{language}-raw.json"));
                    let metadata = dumps::first_metadata(&graph).unwrap();
                    write(
                        &PathBuf::from(value("--output-dir"))
                            .join(format!("{language}-topics.json")),
                        &json!({"metadata":{"language":language,"complete":!self.incomplete_topics,
                    "source":"local_model_on_verified_SQL_dump_outlinks","input_file_sha256":dumps::file_sha256(&graph).unwrap(),"graph_finished_at":metadata["finished_at"],
                    "model":read_json(Path::new(&value("--model-config"))).unwrap(),"predicted_pages":2,"missing_pages":0},"pages":[]}),
                    );
                }
            }
            "fetch-content" => {
                for language in languages() {
                    let database = format!("{language}wiki");
                    let root = PathBuf::from(value("--root")).join(&database);
                    std::fs::create_dir_all(&root).unwrap();
                    let path = root.join("content.xml");
                    std::fs::write(&path, "synthetic XML archive").unwrap();
                    write(
                        &root.join("content-2026-10-01.json"),
                        &json!({"language":language,"database":database,"complete":true,"content_release":"2026-10-01","files":{"content.xml":{"path":path}}}),
                    );
                }
            }
            "quality" => {
                for language in languages() {
                    write(
                        &PathBuf::from(value("--output-dir"))
                            .join(format!("{language}-quality.json")),
                        &json!({"metadata":{"language":language,"complete":true,"content_release":"2026-10-01","quality_revision_rule":"selected_official_xml_revision_v1","definition":"synthetic controller fixture"},"pages":[]}),
                    );
                }
            }
            "analyze" => {
                let language = dumps::first_metadata(Path::new(&args[1])).unwrap()["language"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                self.publish(
                    &[language],
                    Path::new(&value("--output")).parent().unwrap(),
                    Path::new(&args[1]).parent().unwrap(),
                );
            }
            "compare" => {
                let languages = args[1..]
                    .iter()
                    .take_while(|s| !s.starts_with("--"))
                    .map(|p| {
                        dumps::first_metadata(Path::new(p)).unwrap()["language"]
                            .as_str()
                            .unwrap()
                            .to_owned()
                    })
                    .collect::<Vec<_>>();
                self.publish(
                    &languages,
                    Path::new(&value("--output-dir")),
                    Path::new(&args[1]).parent().unwrap(),
                );
            }
            "refresh-measures" => {
                let folder = PathBuf::from(value("--data-dir"));
                let manifest = read_json(&folder.join("manifest.json")).unwrap();
                let languages = manifest
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|e| e["language"].as_str())
                    .filter(|l| *l != "all")
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                self.publish(&languages, &folder, &self.root.join("imported"));
            }
            _ => panic!("Unexpected test command {command}"),
        }
        Ok(())
    }
}

#[test]
fn relational_collection_is_atomic_resumable_and_removes_only_its_own_files() {
    let mut f = Fixture::new();
    f.options.selected.push(("lmo".into(), "lmowiki".into()));
    let destination = f.root.join("collection.sqlite");
    f.options.collection_database = Some(destination.clone());
    std::fs::create_dir_all(&f.options.import_dir).unwrap();
    let collected = f.options.import_dir.join("existing-collected.json");
    std::fs::write(&collected, "preserve existing research").unwrap();
    f.steps.fail = Some("quality".into());
    assert!(collection_workflow(&f.options, &mut f.steps).is_err());
    assert!(!destination.exists());
    let imports = f
        .steps
        .calls
        .iter()
        .filter(|c| c[0] == "import-dumps")
        .collect::<Vec<_>>();
    assert_eq!(imports.len(), 2);
    for call in imports {
        let manifest = Path::new(&call[call.iter().position(|a| a == "--manifest").unwrap() + 1]);
        for path in fetched_outputs(manifest, &read_json(manifest).unwrap())
            .unwrap()
            .into_iter()
            .skip(1)
        {
            assert!(!path.exists());
        }
    }
    f.reset();
    f.steps.fail = None;
    collection_workflow(&f.options, &mut f.steps).unwrap();
    assert!(!f
        .steps
        .calls
        .iter()
        .any(|c| c[0] == "fetch-dumps" || c[0] == "import-dumps" || c[0] == "topics"));
    let db = crate::store::Census::open(&destination).unwrap();
    assert_eq!(db.manifest().unwrap().as_array().unwrap().len(), 3);
    let approved = std::fs::read(&destination).unwrap();
    assert_eq!(
        std::fs::read_to_string(&collected).unwrap(),
        "preserve existing research"
    );
    for base in [
        &f.options.dump_root,
        &f.options.import_dir,
        &f.options.work_dir,
        &f.options.output_dir,
    ] {
        assert!(!std::fs::read_dir(base).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".collection-")));
    }
    f.reset();
    collection_workflow(&f.options, &mut f.steps).unwrap();
    assert!(f.steps.calls.is_empty());
    f.options.force = true;
    f.steps.fail = Some("topics".into());
    assert!(collection_workflow(&f.options, &mut f.steps).is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), approved);
    assert_eq!(db.manifest().unwrap().as_array().unwrap().len(), 3);
}

#[test]
fn relational_cleanup_rejects_manifest_paths_outside_the_work_area() {
    let f = Fixture::new();
    std::fs::create_dir_all(&f.options.dump_root).unwrap();
    let outside = f.root.join("collected-dump.sql.gz");
    std::fs::write(&outside, "existing research").unwrap();
    let manifest = f.options.dump_root.join("manifest.json");
    write(&manifest, &json!({}));
    assert!(remove_downloads(
        &manifest,
        &json!({"files":{"page":{"path":outside}}}),
        &f.options.dump_root
    )
    .is_err());
    assert_eq!(
        std::fs::read_to_string(outside).unwrap(),
        "existing research"
    );
}

#[test]
fn complete_sequence_and_verified_resume_use_native_commands_only() {
    let mut f = Fixture::new();
    f.run().unwrap();
    assert_eq!(
        f.steps
            .calls
            .iter()
            .map(|c| c[0].as_str())
            .collect::<Vec<_>>(),
        [
            "fetch-dumps",
            "import-dumps",
            "topics",
            "fetch-content",
            "quality",
            "analyze"
        ]
    );
    let raw = signature(&f.options.import_dir.join("vec-raw.json"));
    f.reset();
    f.run().unwrap();
    assert!(f.steps.calls.is_empty(), "{:?}", f.steps.calls);
    assert_eq!(signature(&f.options.import_dir.join("vec-raw.json")), raw);
}

#[test]
fn every_selected_edition_reaches_all_collection_stages_before_comparison() {
    let mut f = Fixture::new();
    f.options.selected.push(("lmo".into(), "lmowiki".into()));
    f.run().unwrap();
    assert_eq!(
        f.steps
            .calls
            .iter()
            .map(|c| c[0].as_str())
            .collect::<Vec<_>>(),
        [
            "fetch-dumps",
            "import-dumps",
            "import-dumps",
            "topics",
            "fetch-content",
            "quality",
            "quality",
            "compare"
        ]
    );
    for command in f
        .steps
        .calls
        .iter()
        .filter(|c| matches!(c[0].as_str(), "fetch-dumps" | "fetch-content" | "topics"))
    {
        assert!(command.iter().any(|s| s == "vec"));
        assert!(command.iter().any(|s| s == "lmo"));
    }
    f.reset();
    f.run().unwrap();
    assert!(f.steps.calls.is_empty());
}

#[test]
fn plans_write_nothing_and_include_sql_history_and_current_content() {
    let mut f = Fixture::new();
    f.options.plan = true;
    f.run().unwrap();
    assert_eq!(f.steps.calls.len(), 2);
    assert!(f.steps.calls.iter().all(|c| c.contains(&"--plan".into())));
    assert!(!f.options.dump_root.exists());
    assert!(!f.options.import_dir.exists());
    assert!(!f.options.output_dir.exists());
    assert!(!f.options.log_dir.exists());
}

#[test]
fn failed_children_and_deferred_topics_stop_before_later_stages() {
    let mut f = Fixture::new();
    f.steps.fail = Some("import-dumps".into());
    assert!(f.run().is_err());
    assert_eq!(f.steps.calls.len(), 2);
    f.reset();
    f.steps.fail = None;
    f.steps.incomplete_topics = true;
    assert!(f.run().is_err());
    assert_eq!(
        f.steps
            .calls
            .iter()
            .map(|c| c[0].as_str())
            .collect::<Vec<_>>(),
        ["import-dumps", "topics"]
    );
    assert!(!f.options.output_dir.exists());
    f.reset();
    f.steps.incomplete_topics = false;
    f.run().unwrap();
    assert_eq!(f.steps.calls[0][0], "topics");
}

#[test]
fn history_only_refresh_reuses_sql_and_topics_and_preserves_source_archives() {
    let mut f = Fixture::new();
    f.run().unwrap();
    f.reset();
    let stage = f.options.work_dir.join("vecwiki-20261001.sqlite");
    rusqlite::Connection::open(stage)
        .unwrap()
        .execute(
            "UPDATE history_imported SET sha256='stale' WHERE phase='initial'",
            [],
        )
        .unwrap();
    f.run().unwrap();
    assert_eq!(
        f.steps
            .calls
            .iter()
            .map(|c| c[0].as_str())
            .collect::<Vec<_>>(),
        ["import-history"]
    );
    let history = f.options.import_dir.join("vec-history.json");
    let mut value = read_json(&history).unwrap();
    value["metadata"]["new_measure"] = json!("verified supplement update");
    write(&history, &value);
    f.reset();
    f.run().unwrap();
    assert_eq!(
        f.steps
            .calls
            .iter()
            .map(|c| c[0].as_str())
            .collect::<Vec<_>>(),
        ["refresh-measures"]
    );
}

#[test]
fn extending_acquisition_retains_prior_editions_without_rehashing_their_archives() {
    let mut f = Fixture::new();
    f.run().unwrap();
    let archive = f.options.dump_root.join("vecwiki/page");
    let before = signature(&archive);
    let raw_before = signature(&f.options.import_dir.join("vec-raw.json"));
    f.reset();
    f.options.selected = vec![("lmo".into(), "lmowiki".into())];
    f.run().unwrap();
    assert_eq!(signature(&archive), before);
    assert_eq!(
        signature(&f.options.import_dir.join("vec-raw.json")),
        raw_before
    );
    let fetch = &f.steps.calls[0];
    assert_eq!(fetch.last().map(String::as_str), Some("lmo"));
    assert!(!fetch.iter().any(|s| s == "vec"));
    let compare = f.steps.calls.iter().find(|c| c[0] == "compare").unwrap();
    assert!(compare.iter().any(|s| s.ends_with("vec-raw.json")));
    assert!(compare.iter().any(|s| s.ends_with("lmo-raw.json")));
    f.reset();
    f.run().unwrap();
    assert!(f.steps.calls.is_empty());
}

#[test]
fn changed_category_rules_recompute_observations_and_missing_transports_are_repaired() {
    let mut f = Fixture::new();
    f.run().unwrap();
    f.reset();
    write(
        &f.root.join("config/categories.json"),
        &json!({"min_support":10}),
    );
    f.run().unwrap();
    assert_eq!(
        f.steps
            .calls
            .iter()
            .map(|c| c[0].as_str())
            .collect::<Vec<_>>(),
        ["analyze"]
    );
    let manifest = read_json(&f.options.output_dir.join("manifest.json")).unwrap();
    let latest = manifest.as_array().unwrap().last().unwrap();
    std::fs::remove_file(
        f.options
            .output_dir
            .join(latest["articles_file"].as_str().unwrap()),
    )
    .unwrap();
    f.reset();
    f.run().unwrap();
    assert_eq!(
        f.steps
            .calls
            .iter()
            .map(|c| c[0].as_str())
            .collect::<Vec<_>>(),
        ["refresh-measures"]
    );
}

#[test]
fn legacy_combined_download_checkpoints_seed_per_edition_reuse() {
    let mut f = Fixture::new();
    f.run().unwrap();
    let edition = f.options.selected[0].clone();
    let (manifest, meta) = manifest_for(&f.options, &edition.0, &edition.1).unwrap();
    let outputs = fetched_outputs(&manifest, &meta).unwrap();
    let mut state = StepState::open(f.options.dump_root.join("pipeline-state.json")).unwrap();
    state.steps.remove("fetch:vec");
    state.save("fetch",json!({"languages":[edition],"sql_release":f.options.sql_release,"history_release":f.options.history_release,"dump_root":path_text(&f.options.dump_root)}),&outputs).unwrap();
    f.options.selected.push(("lmo".into(), "lmowiki".into()));
    f.reset();
    f.run().unwrap();
    let downloads = f
        .steps
        .calls
        .iter()
        .filter(|c| c[0] == "fetch-dumps")
        .collect::<Vec<_>>();
    assert_eq!(downloads.len(), 1);
    assert_eq!(downloads[0].last().unwrap(), "lmo");
    let updated = StepState::open(f.options.dump_root.join("pipeline-state.json")).unwrap();
    assert!(updated.steps.contains_key("fetch:vec"));
    assert!(updated.steps.contains_key("fetch:lmo"));
}

#[test]
fn changed_import_checkpoints_are_revalidated_before_reuse() {
    let mut f = Fixture::new();
    f.run().unwrap();
    f.reset();
    let stage = f.options.work_dir.join("vecwiki-20261001.sqlite");
    rusqlite::Connection::open(stage)
        .unwrap()
        .execute(
            "UPDATE imported SET sha256='changed' WHERE table_name='page'",
            [],
        )
        .unwrap();
    f.run().unwrap();
    assert_eq!(
        f.steps
            .calls
            .iter()
            .map(|c| c[0].as_str())
            .collect::<Vec<_>>(),
        ["import-dumps"]
    );
    let mut state = StepState::open(f.options.dump_root.join("pipeline-state.json")).unwrap();
    state.steps["import:vec"]["inputs"]["history_rule"] = json!("old definition");
    write(&state.path, &json!({"version":1,"steps":state.steps}));
    f.reset();
    f.run().unwrap();
    assert!(f.steps.calls.is_empty());
    let manifest = f.options.output_dir.join("manifest.json");
    let before = std::fs::read(&manifest).unwrap();
    write(
        &f.options.dump_root.join("pipeline-state.json"),
        &json!({"version":999,"steps":{}}),
    );
    f.reset();
    assert!(f.run().is_err());
    assert!(f.steps.calls.is_empty());
    assert_eq!(std::fs::read(manifest).unwrap(), before);
}

#[test]
#[cfg(unix)]
fn runner_streams_logs_samples_resources_and_reconciles_failed_worker_groups() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let executable = f.root.join("mock-native-cli");
    let script = r#"#!/bin/sh
shift
shift
sleep 30 &
printf '%s' "$!" > worker.pid
printf 'child stdout\n'
printf 'child stderr\n' >&2
printf '{"event":"span_start","span_id":"abandoned-worker","parent_id":"%s","run_id":"%s","name":"mock.worker","started_ns":1}\n' "$ORPHANWIKI_PARENT_SPAN" "$ORPHANWIKI_RUN_ID" >> "$ORPHANWIKI_SPANS_FILE"
sleep 0.3
exit 7
"#;
    std::fs::write(&executable, script).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut runner = Runner::new(&f.options).unwrap();
    runner.executable = executable;
    let settings = runner.trace.clone();
    let result = runtime::with_tracing(settings, || {
        runtime::phase("pipeline", || {
            runner.run("synthetic failure", &["mock".into()])
        })
    });
    assert!(result.is_err());
    runner.finish(&result).unwrap();
    let report = read_json(runner.report_path.as_ref().unwrap()).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["steps"][0]["returncode"], 7);
    assert!(
        report["steps"][0]["sampled_peak_tree_rss_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(report["steps"][0]["user_cpu_seconds"].as_f64().unwrap() >= 0.0);
    assert!(report["monitor_error"].is_null());
    let files = std::fs::read_dir(&f.options.log_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    let log = std::fs::read_to_string(
        files
            .iter()
            .find(|p| p.extension().is_some_and(|s| s == "log"))
            .unwrap(),
    )
    .unwrap();
    assert!(log.contains("child stdout\n"));
    assert!(log.contains("child stderr\n"));
    let metrics = std::fs::read_to_string(
        files
            .iter()
            .find(|p| p.to_string_lossy().ends_with(".metrics.jsonl"))
            .unwrap(),
    )
    .unwrap();
    let events = metrics
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().filter(|e| e["event"] == "sample").count() >= 3);
    assert!(events.iter().any(|e| e["event"] == "step_start"));
    assert!(events.iter().any(|e| e["event"] == "step_end"));
    let spans = std::fs::read_to_string(&runner.trace.as_ref().unwrap().file)
        .unwrap()
        .lines()
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .collect::<Vec<_>>();
    let closed = spans
        .iter()
        .find(|s| s["span_id"] == "abandoned-worker" && s["event"] == "span_end")
        .unwrap();
    assert_eq!(closed["end_is_detection_bound"], true);
    assert_eq!(closed["returncode"], 7);
    let worker = std::fs::read_to_string(f.root.join("worker.pid")).unwrap();
    let state = std::fs::read_to_string(format!("/proc/{worker}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit_once(')')
                .map(|(_, v)| v.trim_start().starts_with('Z'))
        });
    assert!(
        state.is_none_or(|zombie| zombie),
        "Worker remained alive after its command failed"
    );
}
#[test]
fn benchmark_fingerprints_keep_missing_intervals_models_and_every_edge() {
    let f = Fixture::new();
    let path = f.root.join("result.json");
    let mut record = json!({"metadata":{"complete":true,"language":"vec","engine":"python","started_at":"2026-10-01","dump_manifest":"old","topics":{"model_sha512":"frozen","started_at":"one"}},
        "pages":[{"id":1,"age":null,"quality":0.9859704508633369}],"link_candidates":{"1":{"candidates":[{"id":2,"evidence":[{"language":"lmo","source_id":3},{"language":"it","source_id":4}]}]}}});
    let save = |record: &Value| {
        // The canonical reader intentionally requires metadata first.
        let text = format!(
            "{{\"metadata\":{},\"pages\":{},\"link_candidates\":{}}}",
            record["metadata"], record["pages"], record["link_candidates"]
        );
        std::fs::write(&path, text).unwrap();
    };
    save(&record);
    let expected = canonical_fingerprint(&path, None).unwrap();
    assert_eq!(expected["reference_edges"], 2);
    // Independent frozen Python reference, including its exact float encoding.
    assert_eq!(
        expected["sha256"],
        "b7ca872ba3a0645fa9afcd1f3248e9fe4bcc8dd84dd4daa26b478f646c4e1e72"
    );
    record["metadata"]["engine"] = json!("rust");
    record["metadata"]["dump_manifest"] = json!("new");
    record["metadata"]["topics"]["started_at"] = json!("two");
    save(&record);
    assert_eq!(canonical_fingerprint(&path, None).unwrap(), expected);
    for key in ["missing", "edge", "interval", "model", "float"] {
        let mut changed = record.clone();
        match key {
            "missing" => changed["pages"][0]["age"] = json!(0),
            "edge" => {
                changed["link_candidates"]["1"]["candidates"][0]["evidence"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
            }
            "interval" => changed["metadata"]["started_at"] = json!("2026-10-02"),
            "model" => changed["metadata"]["topics"]["model_sha512"] = json!("changed"),
            _ => changed["pages"][0]["quality"] = json!(0.985970450863337),
        }
        save(&changed);
        assert_ne!(
            canonical_fingerprint(&path, None).unwrap(),
            expected,
            "{key}"
        );
    }
    record["metadata"]["complete"] = json!(false);
    save(&record);
    assert!(canonical_fingerprint(&path, None).is_err());
    assert!(local_output(&f.root, "../outside").is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/passwd", f.root.join("escape")).unwrap();
        assert!(local_output(&f.root, "escape").is_err());
    }
}

#[test]
fn benchmark_manifest_requires_one_complete_member_per_language() {
    let f = Fixture::new();
    std::fs::write(
        f.root.join("member.json"),
        r#"{"metadata":{"complete":true,"language":"vec"},"pages":[{"id":1}]}"#,
    )
    .unwrap();
    let outputs: Vec<BenchmarkOutput> =
        serde_json::from_value(json!([{"path":"index.json","kind":"manifest"}])).unwrap();
    write(
        &f.root.join("index.json"),
        &json!([{"file":"member.json","language":"vec"},{"kind":"aggregate","file":"unused.json"}]),
    );
    assert_eq!(
        benchmark_outputs(&f.root, &outputs).unwrap()["index.json:vec"]["pages"],
        1
    );
    for entries in [
        json!([]),
        json!([{"file":"member.json","language":"lmo"}]),
        json!([{"file":"../member.json","language":"vec"}]),
        json!([{"file":"member.json","language":"vec"},{"file":"member.json","language":"vec"}]),
    ] {
        write(&f.root.join("index.json"), &entries);
        assert!(benchmark_outputs(&f.root, &outputs).is_err());
    }
}

#[test]
fn benchmark_verdict_requires_paired_time_and_memory_excluding_warmups() {
    let samples = (0..4).flat_map(|pair|["baseline","candidate"].map(move |variant|json!({"pair":pair,"variant":variant,"warmup":pair==0,"wall_seconds":if variant=="baseline" {10.0} else if pair==0 {1000.0} else {5.0},"cpu_seconds":8.0,"sampled_peak_tree_rss_bytes":100}))).collect::<Vec<_>>();
    let result = benchmark_summary(&samples, 1.1, 1.1).unwrap();
    assert_eq!(result["status"], "passed");
    assert_eq!(result["paired_speedup"]["median"], 2.0);
    for (key, value, status) in [
        ("wall_seconds", json!(12.0), "inconclusive"),
        ("sampled_peak_tree_rss_bytes", json!(120), "no_improvement"),
        ("sampled_peak_tree_rss_bytes", Value::Null, "inconclusive"),
    ] {
        let mut changed = samples.clone();
        changed.last_mut().unwrap()[key] = value;
        assert_eq!(
            benchmark_summary(&changed, 1.1, 1.1).unwrap()["status"],
            status
        );
    }
    assert_eq!(
        benchmark_summary(&samples, 3.0, 1.1).unwrap()["status"],
        "no_improvement"
    );
    assert!(benchmark_summary(&samples[..7], 1.1, 1.1).is_err());
    let mut duplicate = samples.clone();
    duplicate.push(samples[2].clone());
    assert!(benchmark_summary(&duplicate, 1.1, 1.1).is_err());
}

#[cfg(unix)]
#[test]
fn benchmark_runs_fresh_copies_and_rejects_mismatch_mutation_and_failure() {
    let f = Fixture::new();
    let seed = f.root.join("seed.json");
    let original =
        r#"{"metadata":{"complete":true,"language":"vec"},"pages":[{"id":1,"age":null}]}"#;
    std::fs::write(&seed, original).unwrap();
    let script = f.root.join("run.sh");
    std::fs::write(&script,"test ! -e \"$1/result.json\" || exit 9\ncp \"$1/seed.json\" \"$1/result.json\"\nprintf private > \"$1/seed.json\"\nsleep 0.15\n").unwrap();
    let spec_path = f.root.join("spec.json");
    let mut spec = json!({"version":1,"inputs":[seed,script],"copies":[{"source":seed,"destination":"seed.json"}],
        "baseline":{"cwd":"{repo}","commands":[["/bin/sh","{repo}/run.sh","{run}"]]},
        "candidate":{"cwd":"{repo}","commands":[["/bin/sh","{repo}/run.sh","{run}"]]},
        "outputs":[{"path":"result.json","kind":"canonical"}]});
    write(&spec_path, &spec);
    let options = |name: &str| BenchmarkOptions {
        root: f.root.clone(),
        spec: spec_path.clone(),
        output_dir: f.root.join(name),
        repetitions: 3,
        warmups: 1,
        metrics_interval: 0.1,
        min_speedup: 10000.0,
        max_rss_ratio: 1.1,
    };
    assert_eq!(benchmark(options("accepted-parity")).unwrap(), 2);
    let report_path = f.root.join("accepted-parity/comparison.json");
    let report = read_json(&report_path).unwrap();
    assert_eq!(report["correctness"], "matched");
    let variants = report["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["variant"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        variants,
        [
            "baseline",
            "candidate",
            "candidate",
            "baseline",
            "baseline",
            "candidate",
            "candidate",
            "baseline"
        ]
    );
    assert_eq!(std::fs::read_to_string(&seed).unwrap(), original);
    for sample in report["samples"].as_array().unwrap() {
        let measured = read_json(
            &f.root
                .join("accepted-parity")
                .join(sample["report"].as_str().unwrap()),
        )
        .unwrap();
        assert_eq!(sample["wall_seconds"], measured["steps"][0]["wall_seconds"]);
        assert!(measured["measurement"]["rss"]
            .as_str()
            .unwrap()
            .contains("controller excluded"));
        assert!(sample["sampled_peak_tree_rss_bytes"].as_u64().unwrap() > 0);
    }
    let saved = std::fs::read(&report_path).unwrap();
    assert!(benchmark(options("accepted-parity")).is_err());
    assert_eq!(std::fs::read(&report_path).unwrap(), saved);
    for (name, code, message) in [
        (
            "different",
            "sed 's/null/0/' \"$1/seed.json\" > \"$1/result.json\"",
            "Output differenti",
        ),
        ("failed", "exit 7", "codice di uscita 7"),
        (
            "mutated",
            "printf changed > \"$2\"",
            "Input benchmark modificato",
        ),
    ] {
        spec["candidate"]["commands"] =
            json!([["/bin/sh", "-c", code, "benchmark", "{run}", seed]]);
        write(&spec_path, &spec);
        let mut opts = options(name);
        opts.warmups = 0;
        let error = benchmark(opts).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
        let report = read_json(&f.root.join(name).join("comparison.json")).unwrap();
        assert_eq!(report["status"], "failed");
        assert!(report.get("correctness").is_none());
    }
    spec["copies"][0]["destination"] = json!("../escape");
    write(&spec_path, &spec);
    assert!(benchmark(options("invalid")).is_err());
    assert!(!f.root.join("invalid").exists());
}

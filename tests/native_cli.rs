#![cfg(feature = "cli")]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn standalone_editorial_http_preserves_reviews_and_stops_on_sigterm() {
    use std::io::BufRead;
    use std::process::Stdio;
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let workspace = Workspace::new();
    fs::create_dir_all(workspace.0.join("web/data/vec-connections")).unwrap();
    fs::write(workspace.0.join("web/index.html"), "<h1>Dashboard</h1>").unwrap();
    fs::write(workspace.0.join("web/data/manifest.json"),r#"[{"file":"vec.json","language":"vec","date":"2026-10-01T00:00:00Z","connections_dir":"vec-connections"}]"#).unwrap();
    fs::write(workspace.0.join("web/data/vec-connections/1.json"),r#"{"format":"connections_v1","metadata":{"language":"vec","finished_at":"2026-10-01T00:00:00Z","complete":true},"pages":[{"page_id":1,"orphan":true}],"link_candidates":{"1":{"candidates":[{"page_id":2}]}}}"#).unwrap();
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_orphanwiki"))
            .args(["serve", "--port", "0"])
            .env_clear()
            .env("ORPHANWIKI_WEB_ROOT", "web")
            .current_dir(&workspace.0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    std::io::BufReader::new(server.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let url = line.trim().strip_prefix("Dashboard: ").unwrap();
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client.get(url).send().unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().unwrap(), "<h1>Dashboard</h1>");
    assert!(client.head(url).send().unwrap().bytes().unwrap().is_empty());
    let body = r#"{"dataset":"vec.json","language":"vec","source_id":2,"target_id":1,"status":"fixed","version":0}"#;
    let submit = || {
        client
            .post(format!("{url}/api/reviews"))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .unwrap()
    };
    assert_eq!(submit().status(), 200);
    assert_eq!(submit().status(), 409);
    let response = client
        .get(format!("{url}/api/reviews?dataset=vec.json"))
        .send()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&response.bytes().unwrap()).unwrap();
    assert_eq!(value["reviews"][0]["version"], 1);
    assert_eq!(
        client
            .post(format!("{url}/api/reviews"))
            .header("content-type", "text/plain")
            .body(body)
            .send()
            .unwrap()
            .status(),
        415
    );
    #[cfg(unix)]
    {
        assert!(Command::new("kill")
            .args(["-TERM", &server.0.id().to_string()])
            .status()
            .unwrap()
            .success());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(status) = server.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "SIGTERM did not stop the HTTP server"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

struct Workspace(PathBuf);

#[test]
fn tracing_keeps_scientific_bytes_identical_and_records_nested_native_phases() {
    let w = Workspace::new();
    fs::write(w.0.join("raw.json"),r#"{"metadata":{"language":"vec","complete":true,"demo":true,"finished_at":"2026-10-02T00:00:00Z"},"pages":[{"page_id":1,"title":"A","links":["B"]},{"page_id":2,"title":"B"}]}"#).unwrap();
    fs::write(w.0.join("rules.json"), "{}").unwrap();
    let plain = w.run(&[
        "analyze",
        "raw.json",
        "--rules",
        "rules.json",
        "--output",
        "plain/vec.json",
    ]);
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let trace = w.0.join("trace.jsonl");
    let traced = Command::new(env!("CARGO_BIN_EXE_orphanwiki"))
        .args([
            "analyze",
            "raw.json",
            "--rules",
            "rules.json",
            "--output",
            "traced/vec.json",
        ])
        .current_dir(&w.0)
        .env_clear()
        .env("ORPHANWIKI_SPANS_FILE", &trace)
        .env("ORPHANWIKI_RUN_ID", "test-run")
        .env("ORPHANWIKI_PARENT_SPAN", "controller-parent")
        .output()
        .unwrap();
    assert!(
        traced.status.success(),
        "{}",
        String::from_utf8_lossy(&traced.stderr)
    );
    for name in ["vec.json", "vec.csv"] {
        assert_eq!(
            fs::read(w.0.join("plain").join(name)).unwrap(),
            fs::read(w.0.join("traced").join(name)).unwrap()
        );
    }
    let records = fs::read_to_string(trace)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
        .collect::<Vec<_>>();
    let mut active = std::collections::BTreeMap::new();
    let mut names = std::collections::BTreeSet::new();
    for record in &records {
        assert_eq!(record["run_id"], "test-run");
        let id = record["span_id"].as_str().unwrap();
        names.insert(record["name"].as_str().unwrap());
        if record["event"] == "span_start" {
            let parent = record["parent_id"].as_str().unwrap();
            assert!(parent == "controller-parent" || active.contains_key(parent));
            assert!(active.insert(id, record).is_none());
        } else {
            let start = active.remove(id).unwrap();
            assert_eq!(record["status"], "completed");
            assert!(
                record["finished_ns"].as_u64().unwrap() >= start["started_ns"].as_u64().unwrap()
            );
        }
    }
    assert!(active.is_empty());
    for name in [
        "cli.analyze",
        "analysis.single",
        "analysis.memory",
        "export.csv",
        "dashboard.member",
        "export.publish",
    ] {
        assert!(names.contains(name), "{name}");
    }
}

#[test]
#[cfg(unix)]
fn sigterm_cancels_native_work_with_or_without_tracing_and_preserves_approved_files() {
    use std::io::Write;
    use std::time::{Duration, Instant};
    let w = Workspace::new();
    fs::write(w.0.join("rules.json"), "{}").unwrap();
    let mut raw = std::io::BufWriter::new(fs::File::create(w.0.join("raw.json")).unwrap());
    raw.write_all(br#"{"metadata":{"language":"vec","complete":true,"demo":true,"finished_at":"2026-10-02T00:00:00Z"},"pages":["#).unwrap();
    for id in 1..=150000 {
        if id > 1 {
            raw.write_all(b",").unwrap();
        }
        write!(raw, "{{\"page_id\":{id},\"title\":\"Article {id}\"}}").unwrap();
    }
    raw.write_all(b"]}").unwrap();
    raw.flush().unwrap();
    drop(raw);
    for tracing in [false, true] {
        let folder = w.0.join(if tracing { "traced" } else { "plain" });
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("manifest.json"), "[]").unwrap();
        fs::write(folder.join("previous-generation.json"), "approved bytes").unwrap();
        let trace = w.0.join("trace.jsonl");
        let mut command = Command::new(env!("CARGO_BIN_EXE_orphanwiki"));
        command
            .args(["analyze", "raw.json", "--rules", "rules.json", "--output"])
            .arg(folder.join("vec.json"))
            .current_dir(&w.0)
            .env_clear();
        if tracing {
            command.env("ORPHANWIKI_SPANS_FILE", &trace);
        }
        let mut child = command.spawn().unwrap();
        if tracing {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fs::read_to_string(&trace)
                .unwrap_or_default()
                .contains("analysis.memory")
            {
                assert!(child.try_wait().unwrap().is_none());
                assert!(Instant::now() < deadline, "Native analysis never started");
                std::thread::sleep(Duration::from_millis(2));
            }
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
            .success());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert_eq!(status.code(), Some(143));
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("SIGTERM did not stop native analysis");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            fs::read_to_string(folder.join("manifest.json")).unwrap(),
            "[]"
        );
        assert_eq!(
            fs::read_to_string(folder.join("previous-generation.json")).unwrap(),
            "approved bytes"
        );
        assert_eq!(fs::read_dir(folder).unwrap().count(), 2);
        if tracing {
            assert!(fs::read_to_string(trace)
                .unwrap()
                .contains("\"status\":\"interrupted\""));
        }
    }
}

impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orphanwiki-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_orphanwiki"))
            .args(arguments)
            .env_clear()
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn standalone_help_needs_neither_python_nor_repository_files() {
    let workspace = Workspace::new();
    let output = workspace.run(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "fetch-dumps",
        "fetch-content",
        "import-dumps",
        "import-history",
        "quality",
        "topics",
        "analyze",
        "compare",
        "refresh-measures",
        "dashboard",
        "serve",
        "pipeline",
    ] {
        assert!(help.contains(command));
        assert!(workspace.run(&[command, "--help"]).status.success());
    }
    assert_eq!(fs::read_dir(&workspace.0).unwrap().count(), 0);
}

#[test]
fn bad_arguments_and_pending_stages_never_publish_output() {
    let workspace = Workspace::new();
    assert_eq!(workspace.run(&[]).status.code(), Some(2));
    assert_eq!(
        workspace
            .run(&["topics", "--batch-size", "0"])
            .status
            .code(),
        Some(2)
    );
    let output = workspace.run(&["dashboard", "--data-dir", "should-not-exist"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty());
    assert_eq!(fs::read_dir(&workspace.0).unwrap().count(), 0);
}

#[test]
fn standalone_dashboard_publishes_a_full_generation_without_python_or_repository_config() {
    let workspace = Workspace::new();
    let canonical = r#"{"metadata":{"language":"vec","complete":true,"finished_at":"2026-10-02T00:00:00Z"},"pages":[{"page_id":1,"title":"A","orphan":true}]}"#;
    fs::write(workspace.0.join("vec.json"), canonical).unwrap();
    fs::write(
        workspace.0.join("manifest.json"),
        r#"[{"file":"vec.json","language":"vec","date":"2026-10-02T00:00:00Z"}]"#,
    )
    .unwrap();
    let result = workspace.run(&["dashboard", "--data-dir", "."]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(workspace.0.join("manifest.json")).unwrap()).unwrap();
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
        assert!(workspace
            .0
            .join(manifest[0][key].as_str().unwrap())
            .exists());
    }
    assert_eq!(
        fs::read_to_string(workspace.0.join("vec.json")).unwrap(),
        canonical
    );
}

#[test]
fn explicit_repository_root_loads_only_its_configured_pool() {
    let workspace = Workspace::new();
    let repository = Workspace::new();
    fs::create_dir(repository.0.join("config")).unwrap();
    fs::write(
        repository.0.join("config/languages.json"),
        r#"{"version":1,"languages":["vec","lmo"],"databases":{"lmo":"customwiki"}}"#,
    )
    .unwrap();
    let repo = repository.0.to_str().unwrap();
    let output = workspace.run(&["--repo-root", repo, "topics", "--languages", "en"]);
    assert_eq!(output.status.code(), Some(2));
    let output = workspace.run(&["topics", "--repo-root", repo, "--languages", "vec", "vec"]);
    assert_eq!(output.status.code(), Some(2));
    let output = workspace.run(&["topics", "--repo-root", repo, "--languages", "lmo", "vec"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("Manca un censimento completo"));
    assert_eq!(fs::read_dir(&repository.0).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&workspace.0).unwrap().count(), 0);
}

#[test]
fn standalone_analysis_publishes_every_output_and_preserves_previous_generations() {
    let w = Workspace::new();
    let input = r#"{"metadata":{"language":"vec","complete":true,"demo":true,"finished_at":"2026-10-02T00:00:00Z"},"pages":[{"page_id":1,"title":"A","created_at":"2000-01-01T00:00:00Z","links":["R","R","missing"]},{"page_id":2,"title":"B"},{"page_id":3,"title":"Isolated"}],"redirects":{"R":"B"}}"#;
    fs::write(w.0.join("raw.json"), input).unwrap();
    fs::write(w.0.join("rules.json"), "{}").unwrap();
    let args = [
        "analyze",
        "raw.json",
        "--rules",
        "rules.json",
        "--output",
        "exports/vec.json",
    ];
    let read = |name: &str| -> serde_json::Value {
        serde_json::from_slice(&fs::read(w.0.join(name)).unwrap()).unwrap()
    };
    let run = || {
        let output = w.run(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run();
    let canonical = fs::read(w.0.join("exports/vec.json")).unwrap();
    let csv = fs::read(w.0.join("exports/vec.csv")).unwrap();
    let first = read("exports/manifest.json");
    let rows = read("exports/vec.json");
    assert_eq!(rows["pages"][0]["out_degree"], 1);
    assert_eq!(rows["pages"][1]["in_degree"], 1);
    assert_eq!(rows["pages"][2]["orphan"], true);
    // Give the approved canonical file a reproducible old mtime. Its newly
    // prepared temporary copy must never become the summary's fingerprint.
    std::fs::File::open(w.0.join("exports/vec.json"))
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000)),
        )
        .unwrap();
    run();
    let next = read("exports/manifest.json");
    for key in ["summary_file", "creator_index_file"] {
        let summary = read(&format!("exports/{}", next[0][key].as_str().unwrap()));
        assert_eq!(summary["sources"][0]["mtime_ns"], 1_000_000_000_000u64);
        assert_eq!(summary["sources"][0]["size"], canonical.len());
        assert_eq!(summary["sources"][0]["file"], "vec.json");
    }

    assert_ne!(first[0]["articles_file"], next[0]["articles_file"]);
    for manifest in [&first, &next] {
        for (key, value) in manifest[0].as_object().unwrap() {
            if key.ends_with("_file") || key.ends_with("_dir") {
                assert!(w.0.join("exports").join(value.as_str().unwrap()).exists());
            }
        }
    }
    assert_eq!(fs::read(w.0.join("exports/vec.json")).unwrap(), canonical);
    assert_eq!(fs::read(w.0.join("exports/vec.csv")).unwrap(), csv);
    let approved = fs::read(w.0.join("exports/manifest.json")).unwrap();
    fs::write(w.0.join("raw.json"), input.replace("Isolated", "Changed")).unwrap();
    assert!(!w.run(&args).status.success());
    assert_eq!(
        fs::read(w.0.join("exports/manifest.json")).unwrap(),
        approved
    );
    assert_eq!(fs::read(w.0.join("exports/vec.json")).unwrap(), canonical);
    assert_eq!(fs::read(w.0.join("exports/vec.csv")).unwrap(), csv);
    assert!(!w
        .run(&[
            "analyze",
            "raw.json",
            "--rules",
            "rules.json",
            "--quality",
            "missing.json",
            "--output",
            "new/vec.json"
        ])
        .status
        .success());
    assert!(!w.0.join("new").exists());
    assert!(!w
        .run(&[
            "analyze",
            "raw.json",
            "--rules",
            "rules.json",
            "--output",
            "raw.json"
        ])
        .status
        .success());
    assert!(fs::read_dir(&w.0).unwrap().all(|p| !p
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("orphanwiki-export-")));
}

#[test]
fn standalone_compare_publishes_all_members_and_retains_every_previous_generation() {
    use serde_json::{json, Value};
    let w = Workspace::new();
    fs::create_dir(w.0.join("config")).unwrap();
    fs::write(
        w.0.join("config/languages.json"),
        r#"{"version":1,"languages":["vec","lmo","fr"]}"#,
    )
    .unwrap();
    fs::write(w.0.join("rules.json"), "{}").unwrap();
    let graph = |language: &str, links: &[(usize, usize)]| {
        let meta = json!({"language":language,"database":format!("{language}wiki"),"source":format!("Wikimedia SQL dumps / {language}wiki"),"complete":true,"started_at":"2026-10-01T00:00:00Z","finished_at":"2026-10-02T00:00:00Z","sql_release":"20261001","dump_manifest_sha256":"a".repeat(64),"item_mapping":{"source":"page_props.wikibase_item","complete":true}});
        let pages=(1..=6).map(|id|json!({"page_id":id,"title":format!("{language}-{id}"),"wikibase_item":format!("Q{id}"),"links":links.iter().filter(|(s,_)|*s==id).map(|(_,t)|format!("{language}-{t}")).collect::<Vec<_>>()})).collect::<Vec<_>>();
        fs::write(
            w.0.join(format!("{language}-raw.json")),
            format!("{{\"metadata\":{meta},\"pages\":{}}}", json!(pages)),
        )
        .unwrap();
    };
    graph("vec", &[(2, 4)]);
    graph("lmo", &[(2, 1)]);
    graph("fr", &[(2, 1)]);
    let run = |inputs: &[&str]| {
        let mut args = vec![
            "compare",
            "--rules",
            "rules.json",
            "--work-dir",
            "work",
            "--output-dir",
            "exports",
        ];
        args.extend(inputs);
        let result = w.run(&args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    let read =
        |path: PathBuf| -> Value { serde_json::from_slice(&fs::read(path).unwrap()).unwrap() };
    run(&["vec-raw.json", "lmo-raw.json"]);
    let original = read(w.0.join("exports/manifest.json"));
    assert_eq!(original.as_array().unwrap().len(), 4);
    let old = original
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let p = w.0.join("exports").join(e["file"].as_str().unwrap());
            let b = fs::read(&p).unwrap();
            (p, b)
        })
        .collect::<Vec<_>>();
    run(&["fr-raw.json"]);
    let expanded = read(w.0.join("exports/manifest.json"));
    assert_eq!(expanded.as_array().unwrap().len(), 9);
    let vec = expanded
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["language"] == "vec")
        .unwrap();
    let result = read(w.0.join("exports").join(vec["file"].as_str().unwrap()));
    assert_eq!(result["link_candidates"]["1"]["candidate_count"], 1);
    assert_eq!(
        result["link_candidates"]["1"]["candidates"][0]["evidence"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let aggregate = expanded
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["kind"] == "aggregate")
        .unwrap();
    let summary = read(
        w.0.join("exports")
            .join(aggregate["summary_file"].as_str().unwrap()),
    );
    assert_eq!(summary["census_page_count"], 18);
    assert_eq!(
        summary["metadata"]["languages"],
        json!(["fr", "lmo", "vec"])
    );
    let checkpoint = fs::read(w.0.join("work/comparison-cache/manifest.json")).unwrap();
    run(&["fr-raw.json"]);
    assert_eq!(
        fs::read(w.0.join("work/comparison-cache/manifest.json")).unwrap(),
        checkpoint
    );
    let next = read(w.0.join("exports/manifest.json"));
    assert_eq!(next.as_array().unwrap().len(), 9);
    for (path, bytes) in old {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    for entry in next
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] != "aggregate")
    {
        let canonical = w.0.join("exports").join(entry["file"].as_str().unwrap());
        let info = fs::metadata(&canonical).unwrap();
        let mtime = info
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        for key in ["summary_file", "creator_index_file"] {
            let report = read(w.0.join("exports").join(entry[key].as_str().unwrap()));
            assert_eq!(report["sources"][0]["mtime_ns"], mtime);
            assert_eq!(report["sources"][0]["size"], info.len());
        }
    }
}

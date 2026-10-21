#![cfg(feature = "cli")]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Workspace(PathBuf);

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

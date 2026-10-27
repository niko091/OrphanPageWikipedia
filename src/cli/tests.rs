use super::*;
use serde_json::json;

#[test]
fn commands_accept_the_existing_pipeline_arguments() {
    for arguments in [
        vec![
            "fetch-dumps",
            "--sql-release",
            "20261001",
            "--history-release",
            "2026-09",
            "--languages",
            "lmo",
            "vec",
            "--plan",
        ],
        vec!["fetch-content", "--content-release", "2026-10-01", "--plan"],
        vec![
            "import-dumps",
            "--manifest",
            "manifest.json",
            "--history-output",
            "history.json",
        ],
        vec![
            "import-history",
            "--manifest",
            "manifest.json",
            "--history-cache-mib",
            "1024",
            "--history-commit-rows",
            "100000",
        ],
        vec![
            "import-history",
            "--sql-release",
            "20261001",
            "--history-release",
            "2026-09",
        ],
        vec!["quality", "--content-release", "2026-10-01"],
        vec!["refresh-measures", "--languages", "vec", "lmo"],
        vec![
            "topics",
            "--batch-size",
            "10000",
            "--time-budget-seconds",
            "604800",
        ],
        vec![
            "analyze",
            "vec-raw.json",
            "--history",
            "vec-history.json",
            "--quality",
            "vec-quality.json",
        ],
        vec![
            "compare",
            "vec-raw.json",
            "lmo-raw.json",
            "--topics",
            "vec-topics.json",
            "lmo-topics.json",
            "--histories",
        ],
        vec!["dashboard", "--data-dir", "web/data"],
    ] {
        command()
            .try_get_matches_from(std::iter::once("orphanwiki").chain(arguments))
            .unwrap();
    }
}

#[test]
fn argument_errors_are_distinct_from_backend_errors() {
    for arguments in [
        vec![],
        vec!["unknown"],
        vec!["analyze"],
        vec!["fetch-dumps"],
        vec!["topics", "--batch-size", "0"],
        vec!["topics", "--batch-size", "10001"],
        vec!["topics", "--time-budget-seconds", "NaN"],
        vec!["topics", "--time-budget-seconds", "inf"],
        vec!["topics", "--time-budget-seconds", "0"],
        vec!["import-history", "--manifest", "x", "--languages", "vec"],
        vec![
            "import-history",
            "--manifest",
            "x",
            "--sql-release",
            "20261001",
        ],
        vec![
            "import-history",
            "--sql-release",
            "20261001",
            "--history-release",
            "2026-09",
            "--output",
            "x",
        ],
        vec![
            "import-dumps",
            "--manifest",
            "x",
            "--history-cache-mib",
            "65537",
        ],
        vec![
            "import-dumps",
            "--manifest",
            "x",
            "--history-commit-rows",
            "0",
        ],
    ] {
        let error = command()
            .try_get_matches_from(std::iter::once("orphanwiki").chain(arguments))
            .unwrap_err();
        assert_eq!(error.exit_code(), 2);
    }
    assert_eq!(
        command()
            .try_get_matches_from(["orphanwiki", "--help"])
            .unwrap_err()
            .exit_code(),
        0
    );
}

#[test]
fn complete_language_pool_is_validated_before_selecting_a_subset() {
    let pool = orphanwiki::configured_languages(&json!({"version":1,"languages":["vec","lmo","be-tarask"],"databases":{"lmo":"customwiki","be-tarask":"be_x_oldwiki"}})).unwrap();
    assert_eq!(pool[2].1, "be_x_oldwiki");
    assert_eq!(
        orphanwiki::select_languages(&pool, Some(&["lmo".into(), "vec".into()])).unwrap(),
        pool[..2]
    );
    for selection in [vec![], vec!["en".into()], vec!["vec".into(), "vec".into()]] {
        assert!(orphanwiki::select_languages(&pool, Some(&selection)).is_err());
    }
    for config in [
        json!({"version":1,"languages":["vec","lmo"],"databases":{"lmo":"bad"}}),
        json!({"version":1,"languages":["vec"],"databases":{"en":"enwiki"}}),
        json!({"version":1,"languages":["vec","vec"]}),
        json!({"version":1,"languages":["all"]}),
        json!({"version":1,"languages":["vec"],"sampling":10}),
        json!({"version":1,"languages":[]}),
    ] {
        assert!(orphanwiki::configured_languages(&config).is_err());
    }
    for language in ["", "ALL", "a/b", "é", "all"] {
        assert!(orphanwiki::wiki_database(language, None).is_err());
    }
    assert_eq!(
        orphanwiki::wiki_database("roa-tara", None).unwrap(),
        "roa_tarawiki"
    );
    assert!(orphanwiki::wiki_database("vec", Some("vecwiki_p")).is_err());
}

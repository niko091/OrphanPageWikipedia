use super::parse_insert;
#[cfg(feature = "cli")]
#[test]
fn incremental_cache_writers_share_the_existing_exclusive_lock() {
    let path = std::env::temp_dir().join(format!("orphanwiki-cache-lock-{}", std::process::id()));
    let first = super::PublicationLock::acquire_file(&path).unwrap();
    assert!(super::PublicationLock::acquire_file(&path).is_err());
    drop(first);
    let second = super::PublicationLock::acquire_file(&path).unwrap();
    drop(second);
    std::fs::remove_file(path).unwrap();
}

#[cfg(feature = "cli")]
#[test]
fn publication_rechecks_stat_fingerprints_of_identical_reused_files() {
    let (root, _) = super::export_directory(&std::env::temp_dir()).unwrap();
    let prepared = root.path.join("prepared");
    let live = root.path.join("live");
    std::fs::create_dir(&prepared).unwrap();
    std::fs::create_dir(&live).unwrap();
    std::fs::write(prepared.join("canonical.json"), b"approved").unwrap();
    std::fs::write(live.join("canonical.json"), b"approved").unwrap();
    std::fs::write(prepared.join("manifest.json"), b"new").unwrap();
    std::fs::write(live.join("manifest.json"), b"old").unwrap();
    let mut reused = Vec::new();
    assert!(super::reuse_export(
        &prepared.join("canonical.json"),
        &live.join("canonical.json"),
        &mut reused
    )
    .unwrap());
    std::fs::File::open(live.join("canonical.json"))
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000)),
        )
        .unwrap();
    assert!(
        super::publish_prepared(&prepared, &live, Some(b"old"), &reused, |a, b| {
            std::fs::rename(a, b)
        })
        .is_err()
    );
    assert_eq!(std::fs::read(live.join("manifest.json")).unwrap(), b"old");
    assert_eq!(
        std::fs::read(live.join("canonical.json")).unwrap(),
        b"approved"
    );
    assert!(prepared.join("manifest.json").exists());
}

#[cfg(feature = "cli")]
#[test]
fn publication_failures_keep_the_prior_manifest_and_approved_files() {
    let (root, _) = super::export_directory(&std::env::temp_dir()).unwrap();
    let prepared = root.path.join("prepared");
    let live = root.path.join("live");
    std::fs::create_dir(&prepared).unwrap();
    std::fs::create_dir(&live).unwrap();
    for name in ["new-a.json", "new-b.json", "manifest.json"] {
        std::fs::write(prepared.join(name), name).unwrap();
    }
    std::fs::create_dir(prepared.join("empty")).unwrap();
    std::fs::write(live.join("manifest.json"), b"approved").unwrap();
    std::fs::write(live.join("old.json"), b"canonical").unwrap();
    assert!(
        super::publish_prepared(&prepared, &live, Some(b"stale"), &[], |a, b| {
            std::fs::rename(a, b)
        })
        .is_err()
    );
    let mut moves = 0;
    assert!(
        super::publish_prepared(&prepared, &live, Some(b"approved"), &[], |a, b| {
            moves += 1;
            if moves == 2 {
                Err(std::io::Error::other("synthetic rename failure"))
            } else {
                std::fs::rename(a, b)
            }
        })
        .is_err()
    );
    assert_eq!(moves, 2);
    assert_eq!(
        std::fs::read(live.join("manifest.json")).unwrap(),
        b"approved"
    );
    assert_eq!(std::fs::read(live.join("old.json")).unwrap(), b"canonical");
    assert!(prepared.join("manifest.json").is_file());
}

use super::history_activity;

#[cfg(feature = "cli")]
#[test]
fn job_timestamps_preserve_python_microseconds_and_reject_invalid_dates() {
    for (value, expected) in [
        ("2026-10-02 01:02:03", "2026-10-02T01:02:03+00:00"),
        ("2026-10-02 01:02:03.000", "2026-10-02T01:02:03+00:00"),
        ("2026-10-02 01:02:03.12", "2026-10-02T01:02:03.120000+00:00"),
        (
            "2026-10-02 01:02:03.123456789",
            "2026-10-02T01:02:03.123456+00:00",
        ),
    ] {
        assert_eq!(
            super::history_timestamp(Some(value)).as_deref(),
            Some(expected)
        );
    }
    for value in [
        "invalid",
        "0000-01-01 00:00:00",
        "2026-02-30 00:00:00",
        "2026-01-01 00:00:60",
    ] {
        assert_eq!(super::history_timestamp(Some(value)), None);
    }
    assert_eq!(super::history_timestamp(None), None);
}

#[test]
fn prior_activity_uses_strict_time_and_completed_months() {
    let mut events = Vec::new();
    for day in 1..=5 {
        events.push((format!("2026-01-{day:02} 10:00:00.0"), 0));
        events.push((format!("2026-02-{day:02} 10:00:00.0"), 1));
    }
    events.push(("2026-03-01 12:00:00.0".to_owned(), 0));
    let metrics = history_activity(events, vec!["2026-03-01 12:00:00.0".to_owned()]).unwrap();
    assert_eq!(metrics, vec![(5, 5, 2, "two".to_owned())]);
}

#[test]
fn prior_activity_sweep_preserves_query_order_duplicates_and_time_boundaries() {
    let events = vec![
        ("2026-03-01 12:00:00.0".to_owned(), 0),
        ("2026-01-01 00:00:00.0".to_owned(), -1),
        ("2026-02-01 00:00:00.0".to_owned(), 0),
        ("2026-02-01 00:00:00.0".to_owned(), 1),
    ];
    let queries = [
        "2026-04-01 00:00:00.0",
        "2026-02-01 00:00:00.0",
        "2026-01-01 00:00:00.0",
        "2026-04-01 00:00:00.0",
        "2025-01-01 00:00:00.0",
        "2026-02-01 00:00:00.1",
    ];
    let result =
        history_activity(events, queries.iter().map(|s| (*s).to_owned()).collect()).unwrap();
    let expected = [(2, 2), (0, 1), (0, 0), (2, 2), (0, 0), (1, 2)];
    for (actual, (main, other)) in result.iter().zip(expected) {
        assert_eq!(actual, &(main, other, 0, "none".to_owned()));
    }
    assert_eq!(result.len(), queries.len());
    assert!(history_activity(Vec::new(), vec!["invalid".to_owned()]).is_err());
    assert!(history_activity(vec![("invalid".to_owned(), 0)], Vec::new()).is_err());
    assert_eq!(
        history_activity(Vec::new(), Vec::new()).unwrap(),
        Vec::new()
    );
}

#[test]
fn prior_activity_caches_streaks_without_changing_creation_month_exclusion() {
    let mut events = Vec::new();
    for month in 1..=4 {
        for day in 1..=5 {
            events.push((format!("2026-{month:02}-{day:02} 10:00:00.0"), 0));
        }
    }
    let queries = [
        "2026-04-30 12:00:00.0",
        "2026-02-01 12:00:00.0",
        "2026-04-01 12:00:00.0",
        "2026-05-01 12:00:00.0",
    ];
    let result =
        history_activity(events, queries.iter().map(|s| (*s).to_owned()).collect()).unwrap();
    assert_eq!(
        result,
        vec![
            (20, 0, 3, "three_to_six".to_owned()),
            (6, 0, 1, "one".to_owned()),
            (16, 0, 3, "three_to_six".to_owned()),
            (20, 0, 4, "three_to_six".to_owned()),
        ]
    );
}

#[test]
fn parses_mysql_values_and_escapes() {
    let sql = "INSERT INTO `page` VALUES (1,'A\\'B',NULL,-2),(2,'Vèneto\\n',3,4);\n";
    assert_eq!(
        parse_insert(sql).unwrap(),
        vec![
            vec![
                Some("1".into()),
                Some("A'B".into()),
                None,
                Some("-2".into())
            ],
            vec![
                Some("2".into()),
                Some("Vèneto\n".into()),
                Some("3".into()),
                Some("4".into())
            ]
        ]
    );
}

#[test]
fn parses_multiline_dump_tuple() {
    assert_eq!(
        parse_insert("(5,0,'Łéngua_vèneta',0,NULL),\n").unwrap(),
        vec![vec![
            Some("5".into()),
            Some("0".into()),
            Some("Łéngua_vèneta".into()),
            Some("0".into()),
            None
        ]]
    );
}

#[test]
fn rejects_truncated_or_malformed_input() {
    for sql in [
        "INSERT INTO `page` VALUES (1,'broken",
        "INSERT INTO `page` VALUES (1,)",
    ] {
        assert!(parse_insert(sql).is_err(), "{sql}");
    }
}

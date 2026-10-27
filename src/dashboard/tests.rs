#[cfg(not(feature = "cli"))]
use super::summary::summary_bridge_float;
use super::interests::{InterestCandidate, InterestEdit, InterestInput};
use super::*;
#[test]
fn chart_float_ingestion_preserves_the_frozen_bridge_without_mutating_observations() {
    let input = serde_json::json!({"page_id":1,"title":"Exact input","orphan":true,
        "quality_status":"scored","quality_score":0.9859704508633369});
    let before = input.clone();
    let mut summary = SummaryAccumulator::new("vec".into(), false, false);
    summary.add_observation(&input, None).unwrap();
    let report: serde_json::Value = serde_json::from_str(&summary.finish().unwrap()).unwrap();
    assert_eq!(
        report["profiles"]["thresholds"]["vec"][2]
            .as_f64()
            .unwrap()
            .to_bits(),
        0.9859704508633368f64.to_bits()
    );
    assert_eq!(input, before);
    assert_eq!(
        input["quality_score"].as_f64().unwrap().to_bits(),
        0.9859704508633369f64.to_bits()
    );
}

#[cfg(not(feature = "cli"))]
#[test]
fn chart_bridge_matches_the_original_parser_on_finite_binary64_inputs() {
    let mut bits = 918_271_613u64;
    for _ in 0..100_000 {
        bits = bits.wrapping_mul(6364136223846793005).wrapping_add(1);
        let value = f64::from_bits(bits);
        let Some(number) = serde_json::Number::from_f64(value) else {
            continue;
        };
        let encoded = crate::dumps::python_float(&number).unwrap();
        let original = serde_json::from_str::<f64>(&encoded);
        let bridged = summary_bridge_float(&number);
        match (original, bridged) {
            (Ok(a), Ok(b)) => assert_eq!(a.to_bits(), b.to_bits(), "{encoded}"),
            (Err(_), Err(_)) => {}
            pair => panic!("bridge and reference disagree for {encoded}: {pair:?}"),
        }
    }
}
#[test]
fn decoded_observations_ignore_incidental_languages_and_scope_verified_members() {
    let page = serde_json::json!({"page_id":1,"title":"A","orphan":true,
        "language":{"not":"a member identity"},"history_creator_key":"history:7",
        "creator_prior_edits_main_history":3,"unused":[1,2,3]});
    let mut member = SummaryAccumulator::new("vec".into(), false, true);
    member.add_observation(&page, None).unwrap();
    assert!(member.add_observation(&page, None).is_err());
    assert_eq!(member.len(), 1);
    let report: serde_json::Value = serde_json::from_str(&member.finish().unwrap()).unwrap();
    assert_eq!(report["overview"]["attributed_orphans"], 1);
    assert_eq!(report["creators"][0]["key"], "history:7");
    let mut aggregate = SummaryAccumulator::new("all".into(), true, true);
    aggregate.add_observation(&page, Some("vec")).unwrap();
    aggregate.add_observation(&page, Some("lmo")).unwrap();
    assert_eq!(aggregate.len(), 2);
    let report: serde_json::Value = serde_json::from_str(&aggregate.finish().unwrap()).unwrap();
    assert_eq!(report["creators"].as_array().unwrap().len(), 2);
    assert_eq!(report["overview"]["total"], 2);
    assert!(aggregate
        .add_observation(&serde_json::json!([]), None)
        .is_err());
    assert!(aggregate
        .add_observation(
            &serde_json::json!({"page_id":2,"title":"","orphan":true}),
            None
        )
        .is_err());
    assert_eq!(aggregate.len(), 2);
}
#[test]
fn interest_score_prioritizes_topics_and_uses_language_only_in_aggregate() {
    let report = interest_scores(InterestInput {
        edits: vec![
            InterestEdit {
                language: "it".into(),
                topic_labels: Some(vec!["Culture.Media".into()]),
            },
            InterestEdit {
                language: "lmo".into(),
                topic_labels: Some(vec!["Culture.Media".into()]),
            },
            InterestEdit {
                language: "lmo".into(),
                topic_labels: None,
            },
        ],
        candidates: vec![
            InterestCandidate {
                key: "vec:1".into(),
                language: "vec".into(),
                topic_labels: Some(vec!["Culture.Media".into()]),
            },
            InterestCandidate {
                key: "lmo:2".into(),
                language: "lmo".into(),
                topic_labels: Some(vec!["Geography".into()]),
            },
        ],
        aggregate: true,
    });
    assert_eq!(report.total_edits, 3);
    assert_eq!(report.known_topic_edits, 2);
    assert_eq!(report.scores[0].topic, 1.0);
    assert_eq!(report.scores[0].language, 0.0);
    assert_eq!(report.scores[1].topic, 0.0);
    assert_eq!(report.scores[1].language, 2.0 / 3.0);
}
#[test]
fn histogram_shared_bins_missing_denominators_and_inclusive_endpoint() {
    let report = numeric(NumericInput {
        rows: vec![
            (true, Some(0.0)),
            (true, Some(1.0)),
            (true, None),
            (false, Some(0.5)),
        ],
        logarithmic: false,
        age: false,
    });
    assert_eq!(report.groups[0].counts[1], 1);
    assert_eq!(report.groups[0].counts[18], 1);
    assert_eq!(report.groups[0].missing, 1);
    assert_eq!(report.groups[0].percentages[1], Some(100.0 / 3.0));
    assert_eq!(report.groups[0].cumulative.last(), Some(&[1.0, 1.0]));
    assert_eq!(report.groups[1].median, Some(0.5));
}
#[test]
fn histogram_groups_both_tails_without_losing_observations() {
    let report = numeric(NumericInput {
        rows: (0..=100)
            .map(|value| (value % 2 == 0, Some(value as f64)))
            .collect(),
        logarithmic: false,
        age: false,
    });
    assert_eq!(report.bin_edges[1], 2.0);
    assert_eq!(report.bin_edges[19], 98.0);
    assert_eq!(report.groups[0].counts[0], 1);
    assert_eq!(report.groups[0].counts[19], 1);
    assert_eq!(report.groups[1].counts[0], 1);
    assert_eq!(report.groups[1].counts[19], 1);
    for group in &report.groups {
        assert_eq!(group.counts.iter().sum::<usize>(), group.known);
    }
    assert_eq!(report.cumulative_ticks[4], 100.0);
}
#[test]
fn numeric_empty_groups_have_no_percentages_or_fabricated_values() {
    let report = numeric(NumericInput {
        rows: vec![(true, None)],
        logarithmic: true,
        age: false,
    });
    assert!(!report.available);
    assert_eq!(report.groups[1].total, 0);
    assert!(report.groups[1].percentages.iter().all(Option::is_none));
    assert_eq!(report.groups[0].median, None);
}
#[test]
fn profiles_use_local_medians_multilabel_or_and_known_empty_labels() {
    let report = profiles(vec![
        (
            "vec".into(),
            true,
            Some(vec![
                "Culture.Media.Music".into(),
                "Culture.Media.Films".into(),
            ]),
            Some(1.0),
            Some(4.0),
            None,
        ),
        (
            "vec".into(),
            false,
            Some(vec![]),
            Some(3.0),
            Some(8.0),
            None,
        ),
        ("lmo".into(), true, None, Some(100.0), None, None),
    ])
    .unwrap();
    assert_eq!(report.topics.rows.len(), 64);
    assert_eq!(report.macrothemes.rows.len(), 4);
    let culture = report
        .macrothemes
        .rows
        .iter()
        .find(|r| r.label == "Culture")
        .unwrap();
    assert_eq!(culture.orphans, 1);
    assert_eq!(culture.known, 2);
    assert_eq!(culture.percent, Some(100.0));
    assert_eq!(report.thresholds["vec"], [Some(2.0), Some(6.0), None]);
    assert_eq!(report.features.rows[0].orphans, 1);
    assert_eq!(report.features.rows[1].orphans, 1);
    assert_eq!(report.features.rows[2].known, 0);
    assert_eq!(report.features.rows.len(), 6);
    assert_eq!(report.features.rows[4].known, 2);
    assert_eq!(report.features.rows[5].percent, Some(0.0));
}

#[test]
fn quality_features_split_at_local_medians_and_missing_is_unknown() {
    let report = profiles(vec![
        ("vec".into(), true, None, None, None, Some(0.1)),
        ("vec".into(), false, None, None, None, Some(0.3)),
        ("lmo".into(), true, None, None, None, Some(0.8)),
        ("lmo".into(), false, None, None, None, None),
    ])
    .unwrap();
    assert_eq!(report.thresholds["vec"][2], Some(0.2));
    assert_eq!(report.thresholds["lmo"][2], Some(0.8));
    assert_eq!(report.features.rows[2].known, 3);
    assert_eq!(report.features.rows[2].total, 2);
    assert_eq!(report.features.rows[2].orphans, 1);
    assert_eq!(report.features.rows[3].total, 1);
    let scores = quality_scores(
        &[[10000, 15, 10, 100, 2, 5], [0; 6]],
        [100.0, 0.15, 0.1, 0.1, 2.0, 5.0],
    )
    .unwrap();
    assert!((scores[0] - 0.998).abs() < 1e-12);
    assert_eq!(scores[1], 0.0);
    assert!(quality_scores(&[], [0.0; 6]).is_err());
}
#[test]
fn aggregate_creators_remain_scoped_and_empty_bot_groups_undefined() {
    let report = creator_rows(vec![
        ("vec:1".into(), None, Some(2), true, vec!["vec".into()]),
        ("lmo:1".into(), None, None, false, vec!["lmo".into()]),
    ]);
    assert_eq!(report.len(), 2);
    assert_eq!(report[0].key, "lmo:1");
    let report = origins(vec![("unknown".into(), None, true)]);
    assert_eq!(report.bots[0].percent, None);
    assert_eq!(report.bots[2].percent, Some(100.0));
}
#[test]
fn bridge_rejects_unknown_operations_and_invalid_json() {
    assert!(calculate("other", "[]").is_err());
    assert!(calculate("numeric", "{}").is_err());
    assert!(calculate("overview", "invalid").is_err());
    let report: serde_json::Value =
        serde_json::from_str(&calculate("overview", "[[true,0,null]]").unwrap()).unwrap();
    assert_eq!(report["orphans"], 1);
    assert_eq!(report["distinct_creators"], 0);
}

#[test]
fn prepared_age_distribution_and_features_require_reconstructed_creation() {
    let mut prepared = SummaryAccumulator::new("vec".into(), false, false);
    prepared
        .add_json(
            r#"[{"page_id":1,"title":"New","orphan":true,
                  "page_created_at":"2020-01-01T00:00:00Z","age_days":365.25,
                  "history_first_revision_age_days":9000},
                 {"page_id":2,"title":"Old","orphan":false,
                  "page_created_at":"2019-01-01T00:00:00Z","age_days":730.5,
                  "history_first_revision_age_days":9000},
                 {"page_id":3,"title":"Missing creation","orphan":true,
                  "age_days":9000,"history_first_revision_age_days":9000},
                 {"page_id":4,"title":"Missing age","orphan":false,
                  "page_created_at":"2020-01-01T00:00:00Z",
                  "history_first_revision_age_days":9000}]"#,
        )
        .unwrap();
    let report: serde_json::Value = serde_json::from_str(&prepared.finish().unwrap()).unwrap();
    let ages = &report["numeric"]["age_days"];
    assert_eq!(ages["known"], 2);
    assert_eq!(ages["groups"][0]["median"], 1.0);
    assert_eq!(ages["groups"][1]["median"], 2.0);
    assert_eq!(ages["groups"][0]["missing"], 1);
    assert_eq!(ages["groups"][1]["missing"], 1);
    assert!(report["numeric"]
        .get("history_first_revision_age_days")
        .is_none());
    let features = report["profiles"]["features"]["rows"].as_array().unwrap();
    for label in ["new", "old"] {
        let row = features.iter().find(|row| row["label"] == label).unwrap();
        assert_eq!(row["known"], 2);
        assert_eq!(row["total"], 1);
        assert_eq!(row["orphans"], usize::from(label == "new"));
    }
}

#[test]
fn prepared_summary_keeps_historical_attribution_distinct_and_complete() {
    let mut prepared = SummaryAccumulator::new("all".into(), true, true);
    prepared
        .add_json(
            r#"[{"page_id":1,"title":"Alpha","language":"vec","orphan":true,
                  "category":"People","topic_status":"predicted","topic_labels":["Culture.Media.Music"],
                  "length_bytes":100,"out_degree":0,"history_creator_key":"history:7",
                  "creator_prior_edits_main_history":3,"creator_groups_at_creation_history":["sysop"]},
                 {"page_id":1,"title":"Beta","language":"lmo","orphan":false,
                  "category":"People","topic_status":"predicted","topic_labels":[],
                  "length_bytes":200,"out_degree":1,"history_creator_key":"history:7",
                  "creator_prior_edits_main_history":5}]"#,
        )
        .unwrap();
    assert_eq!(prepared.len(), 2);
    let report: serde_json::Value = serde_json::from_str(&prepared.finish().unwrap()).unwrap();
    assert_eq!(report["overview"]["total"], 2);
    assert_eq!(report["overview"]["attributed_orphans"], 1);
    assert_eq!(report["creators"].as_array().unwrap().len(), 2);
    assert_eq!(report["numeric"]["creator_prior_edits_main"]["known"], 2);
    assert_eq!(report["numeric"]["creator_prior_articles"]["known"], 0);
    assert_eq!(
        report["numeric"]["creator_articles_created_total"]["known"],
        2
    );
    assert_eq!(
        report["profiles"]["macrothemes"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(prepared
        .add_json(r#"[{"page_id":1,"title":"Duplicate","language":"vec","orphan":true}]"#)
        .is_err());
}

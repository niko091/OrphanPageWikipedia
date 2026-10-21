//! Scientific row joins are available without Python, Wasm or the dump CLI.
use _native::{
    classify_categories, initialize_creator_rows, join_history_rows, join_quality_rows,
    join_topic_rows, observation_ages, observation_row,
};
use serde_json::{json, Value};
use std::collections::HashMap;

fn pages() -> Vec<Value> {
    (1..=6)
        .map(|id| {
            json!({"page_id":id,"title":format!("È {id} 🌙"),"revision_id":id+100,
        "wikibase_item":format!("Q{id}"),"length_bytes":if id==1 {Some(0)} else {None},
        "created_at":"2001-01-01T00:00:00Z","page_created_at":null,
        "categories":["Specific","General","Specific"]})
        })
        .collect()
}
fn observations() -> Vec<Value> {
    let pages = pages();
    let choices = classify_categories(
        &pages
            .iter()
            .map(|page| {
                (
                    page["page_id"].as_i64().unwrap(),
                    vec!["Specific".into(), "General".into(), "Specific".into()],
                )
            })
            .collect::<Vec<_>>(),
        &HashMap::from([("Specific".into(), vec!["General".into()])]),
        (
            vec![],
            Default::default(),
            Default::default(),
            Default::default(),
            5.0,
        ),
    )
    .unwrap();
    pages
        .iter()
        .zip(&choices)
        .map(|(page, choice)| observation_row(page, choice, 0, 1, false).unwrap())
        .collect()
}
fn histories() -> HashMap<i64, Value> {
    [json!({"page_id":1,"page_created_at":"2026-09-30T12:00:00Z","article_created_at":"2000-01-01T00:00:00Z",
        "creator_attribution":"historical_first_revision","creator_history_status":"observed_surviving_history",
        "creator_key":"vec:7","creator_account_type":"registered","creator_is_bot_now":false,
        "creator_registered_at":"2026-09-30T12:00:00Z","creator_tenure_days":0.0,"creator_prior_articles":0,
        "creator_prior_edits_main_history":0,"creator_prior_edits_other_history":0,
        "creator_continuity_months_before_creation":0,"creator_continuity_bucket_before_creation":"none",
        "creator_groups_at_creation":["sysop"],"origin":"translation_tagged"}),
    json!({"page_id":2,"page_created_at":null,"article_created_at":"2000-01-01T00:00:00Z",
        "creator_attribution":"ambiguous","creator_history_status":"unavailable"}),
    json!({"page_id":3,"page_created_at":"2040-01-01T00:00:00Z","article_created_at":"2000-01-01T00:00:00Z",
        "creator_history_status":"observed_surviving_history","creator_is_bot_now":true,"creator_tenure_days":null}),
    json!({"page_id":4,"page_created_at":"2020-01-01T00:00:00Z","article_created_at":null}),
    json!({"page_id":6,"article_created_at":"2025-01-01T00:00:00Z","creator_history_status":"observed_surviving_history"})]
        .into_iter().map(|row|(row["page_id"].as_i64().unwrap(),row)).collect()
}

#[test]
fn native_observations_keep_category_evidence_zero_length_and_explicit_nulls() {
    let rows = observations();
    let row = &rows[0];
    assert_eq!(row["category"], "Specific");
    assert_eq!(row["category_candidates"].as_array().unwrap().len(), 1);
    assert_eq!(row["category_candidates"][0]["support"], 6);
    assert_eq!(row["category_status"], "heuristic_review_needed");
    assert_eq!(row["length_bytes"], 0);
    assert!(rows[1]["length_bytes"].is_null());
    assert_eq!(row["first_public_revision_at"], "2001-01-01T00:00:00Z");
    assert!(row["page_created_at"].is_null());
    assert_eq!(row["in_degree"], 0);
    assert_eq!(row["out_degree"], 1);
    assert_eq!(row["orphan"], true);
    let mut page = pages().remove(0);
    page["first_public_revision_at"] = Value::Null;
    let manual = (Some("Specific".into()), vec![], true);
    let actual = observation_row(&page, &manual, 2, 0, true).unwrap();
    assert!(actual["first_public_revision_at"].is_null());
    assert!(actual["created_at"].is_null());
    assert_eq!(actual["category_status"], "reviewed");
    assert_eq!(actual["category_reason"], "Scelta manuale verificata");
    assert_eq!(actual["orphan"], false);
    let empty = observation_row(&page, &(None, vec![], false), 0, 0, false).unwrap();
    assert!(empty["category"].is_null());
    assert_eq!(empty["category_status"], "no_eligible_category");
}

#[test]
fn history_preserves_reconstruction_first_revision_attribution_and_missing_experience() {
    let mut rows = observations();
    let before = rows.clone();
    let metadata = initialize_creator_rows(&mut rows).unwrap();
    assert_eq!(metadata["unmatched_pages"], 6);
    assert_eq!(metadata["status"], "not_collected");
    assert_eq!(
        join_history_rows(&mut rows, &histories(), "missing_page").unwrap(),
        3
    );
    observation_ages(&mut rows, "2026-10-01T00:00:00Z", true).unwrap();
    let known = &rows[0];
    assert_eq!(known["age_days"], 0.5);
    assert!(known["history_first_revision_age_days"].as_f64().unwrap() > 9000.0);
    assert_eq!(known["creator_tenure_days"], 0.0);
    assert_eq!(known["creator_prior_articles"], 0);
    assert_eq!(known["creator_is_bot_now"], false);
    assert_eq!(
        known["creator_groups_at_creation_history"],
        json!(["sysop"])
    );
    assert_eq!(known["creator_continuity_months_before_creation"], 0);
    assert_eq!(known["creator_prior_edits_main_history"], 0);
    assert!(known["creator_key"].is_null());
    assert!(known["creator_prior_edits_main"].is_null());
    assert!(known["creator_account_type_at_creation"].is_null());
    assert_eq!(known["history_creator_key"], "vec:7");
    assert_eq!(known["origin"], "unknown");
    assert_eq!(known["creator_data_status"], "not_collected");
    assert!(known["creator_orphans_at_30_days"].is_null());
    assert_eq!(
        known["creator_orphans_at_30_days_status"],
        "unavailable_no_historical_link_graph"
    );
    assert!(rows[1]["age_days"].is_null());
    assert!(rows[1]["history_first_revision_age_days"].is_number());
    assert_eq!(rows[1]["history_data_status"], "unattributed_history");
    assert!(rows[2]["age_days"].is_null());
    assert_eq!(rows[2]["creation_date_status"], "after_graph_collection");
    assert_eq!(rows[2]["creator_is_bot_now"], true);
    assert_eq!(
        rows[2]["creator_tenure_status"],
        "missing_or_inconsistent_registration"
    );
    assert!(rows[3]["age_days"].is_number());
    assert!(rows[3]["history_first_revision_age_days"].is_null());
    assert_eq!(
        rows[3]["history_data_status"],
        "history_initial_revision_unknown"
    );
    assert_eq!(rows[4]["history_data_status"], "missing_page");
    assert!(rows[4]["age_days"].is_null());
    for (row, original) in rows.iter().zip(before) {
        for field in [
            "page_id",
            "title",
            "revision_id",
            "in_degree",
            "out_degree",
            "category_candidates",
            "length_bytes",
        ] {
            assert_eq!(row[field], original[field]);
        }
    }
}

#[test]
fn history_and_ages_have_identical_results_across_bounded_batches() {
    let source = observations();
    let history = histories();
    let run = |size| {
        let mut rows = source.clone();
        let mut matched = 0;
        for batch in rows.chunks_mut(size) {
            initialize_creator_rows(batch).unwrap();
            let records = batch
                .iter()
                .filter_map(|row| history.get(&row["page_id"].as_i64().unwrap()).cloned())
                .map(|row| (row["page_id"].as_i64().unwrap(), row))
                .collect();
            matched += join_history_rows(batch, &records, "missing_page").unwrap();
            observation_ages(batch, "2026-10-01T00:00:00Z", true).unwrap();
        }
        (rows, matched)
    };
    let expected = run(6);
    for size in 1..=7 {
        assert_eq!(run(size), expected);
    }
    let mut legacy = source.clone();
    initialize_creator_rows(&mut legacy).unwrap();
    join_history_rows(&mut legacy, &HashMap::new(), "not_collected").unwrap();
    observation_ages(&mut legacy, "2026-10-01T00:00:00Z", false).unwrap();
    assert!(legacy[0]["age_days"].as_f64().unwrap() > 9000.0);
    assert_eq!(legacy[0]["history_data_status"], "not_collected");
    observation_ages(&mut legacy, "2026-10-01T00:00:00Z", true).unwrap();
    assert!(legacy.iter().all(|row| row["age_days"].is_null()));
}

fn quality() -> HashMap<i64, Value> {
    (1..=6).map(|id| {
        let (content,relation,status,score)=match id {
            1=>(Some(101),"same","scored",Some(0.0)),
            2=>(Some(999),"different","scored",Some(0.998001)),
            3=>(None,"unavailable","missing_current_page",None),
            4=>(Some(104),"same","missing_wikitext",None),
            _=>(Some(999),"different","missing_wikitext",None),
        };
        (id,json!({"page_id":id,"revision_id":id+100,"content_revision_id":content,"quality_revision_relation":relation,"quality_status":status,"quality_score":score}))
    }).collect()
}

#[test]
fn quality_retains_zero_missing_and_different_official_revision_and_rejects_contradictions() {
    let mut rows = observations();
    let before = rows.clone();
    join_quality_rows(&mut rows, &quality()).unwrap();
    assert_eq!(rows[0]["quality_score"], 0.0);
    assert_eq!(rows[1]["quality_score"], 0.998001);
    assert_eq!(rows[1]["content_revision_id"], 999);
    assert_eq!(rows[1]["revision_id"], 102);
    assert!(rows[2]["quality_score"].is_null());
    assert_eq!(rows[2]["quality_status"], "missing_current_page");
    for (row, original) in rows.iter().zip(before) {
        for (field, value) in original.as_object().unwrap() {
            assert_eq!(&row[field], value);
        }
    }
    for (key, value) in [
        ("revision_id", json!(900)),
        ("page_id", json!(900)),
        ("content_revision_id", json!(null)),
        ("content_revision_id", json!(0)),
        ("content_revision_id", json!(101.0)),
        ("content_revision_id", json!(true)),
        ("quality_score", json!(-0.001)),
        ("quality_score", json!(0.998002)),
        ("quality_score", json!(true)),
        ("quality_status", json!("missing_current_page")),
        ("quality_status", json!("missing_wikitext")),
        ("quality_revision_relation", json!("different")),
        ("quality_revision_relation", json!("unavailable")),
        ("quality_revision_relation", json!("unknown")),
    ] {
        let mut records = quality();
        records.get_mut(&1).unwrap()[key] = value;
        assert!(
            join_quality_rows(&mut observations(), &records).is_err(),
            "{key}"
        );
    }
    let mut records = quality();
    records.remove(&2);
    assert!(join_quality_rows(&mut observations(), &records).is_err());
}

#[cfg(feature = "cli")]
#[test]
fn imported_json_quality_keeps_the_exact_source_float_through_the_join() {
    let record:Value=serde_json::from_str(r#"{"page_id":1,"revision_id":101,"content_revision_id":101,"quality_revision_relation":"same","quality_status":"scored","quality_score":0.9859704508633369}"#).unwrap();
    let mut rows = observations();
    join_quality_rows(&mut rows[..1], &HashMap::from([(1, record)])).unwrap();
    assert_eq!(
        rows[0]["quality_score"].as_f64().unwrap().to_bits(),
        0.9859704508633369f64.to_bits()
    );
}

#[test]
fn invalid_observations_and_dates_are_errors_and_future_first_revision_is_independent() {
    let choice = (None, vec![], false);
    assert!(observation_row(&json!({}), &choice, 0, 0, false).is_err());
    let mut invalid = choice.clone();
    invalid.1.push(("X".into(), f64::NAN, 1, 1.0));
    assert!(observation_row(&pages()[0], &invalid, 0, 0, false).is_err());
    assert!(initialize_creator_rows(&mut [Value::Null]).is_err());
    let mut history = histories();
    history.get_mut(&1).unwrap()["page_id"] = json!(2);
    assert!(join_history_rows(&mut observations(), &history, "missing_page").is_err());
    let mut rows = observations();
    rows[0]["history_first_revision_at"] = json!("invalid");
    let before = rows.clone();
    assert!(observation_ages(&mut rows, "2026-10-01T00:00:00Z", true).is_err());
    assert_eq!(rows, before);
    rows[0]["history_first_revision_at"] = json!("2040-01-01T00:00:00Z");
    rows[0]["page_created_at"] = json!("2026-09-30T23:59:59.500Z");
    observation_ages(&mut rows, "2026-10-01T00:00:00Z", true).unwrap();
    assert_eq!(rows[0]["age_days"], json!(0.5 / 86400.0));
    assert!(rows[0]["history_first_revision_age_days"].is_null());
    assert_eq!(rows[0]["creation_date_status"], "not_collected");
}

#[test]
fn topic_row_join_keeps_known_negatives_separate_from_missing_and_checks_all_identities() {
    let spec: Value = serde_json::from_str(include_str!("../config/topics.json")).unwrap();
    let taxonomy = spec["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    let records=(1..=6).map(|id| (id,json!({"page_id":id,"revision_id":id+100,
        "topic_labels":if id==1 {json!([])} else if id==2 {json!([spec["labels"][0]])} else {Value::Null},
        "topic_input_count":if id<=2 {1} else {0},"topic_status":if id<=2 {"predicted"} else if id==3 {"no_model_vocabulary"} else {"no_mapped_outlinks"}}))).collect::<HashMap<_,_>>();
    let mut rows = observations();
    assert_eq!(join_topic_rows(&mut rows, &records, &taxonomy).unwrap(), 2);
    assert_eq!(rows[0]["topic_labels"], json!([]));
    assert_eq!(rows[0]["topic_status"], "predicted");
    assert!(rows[2]["topic_labels"].is_null());
    assert_eq!(rows[2]["topic_status"], "no_model_vocabulary");
    for (key, value) in [
        ("page_id", json!(99)),
        ("revision_id", json!(99)),
        ("topic_labels", Value::Null),
        ("topic_labels", json!(["Unknown.Topic"])),
        (
            "topic_labels",
            json!([spec["labels"][0], spec["labels"][0]]),
        ),
        ("topic_labels", json!(false)),
        ("topic_input_count", json!(true)),
        ("topic_input_count", json!(0)),
        ("topic_input_count", json!(-1)),
        ("topic_input_count", json!(1.0)),
        ("topic_status", json!("missing")),
    ] {
        let mut bad = records.clone();
        bad.get_mut(&1).unwrap()[key] = value;
        assert!(
            join_topic_rows(&mut observations(), &bad, &taxonomy).is_err(),
            "{key}"
        );
    }
    let mut missing = records;
    missing.remove(&3);
    assert!(join_topic_rows(&mut observations(), &missing, &taxonomy).is_err());
    assert!(join_topic_rows(&mut observations(), &missing, &Default::default()).is_err());
}

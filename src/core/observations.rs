use super::*;

pub(super) fn timestamp(value: &str) -> Result<chrono::DateTime<chrono::FixedOffset>, String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|error| format!("Invalid timestamp {value:?}: {error}"))
}

/// Preserve missing dates and report future dates separately from known ages.
pub fn article_ages(
    reference: &str,
    created: &[Option<String>],
) -> Result<Vec<(Option<f64>, bool)>, String> {
    let reference = timestamp(reference)?;
    created
        .iter()
        .map(|value| match value {
            None => Ok((None, false)),
            Some(value) => {
                let date = timestamp(value)?;
                let duration = reference.signed_duration_since(date);
                let days = duration.num_seconds() as f64 / 86400.0
                    + duration.subsec_nanos() as f64 / 86_400_000_000_000.0;
                Ok((if days >= 0.0 { Some(days) } else { None }, days < 0.0))
            }
        })
        .collect()
}

/// Compare instants while preserving the original boundary timestamp strings.
pub fn collection_interval(intervals: &[(String, String)]) -> Result<(String, String), String> {
    let mut earliest = None;
    let mut latest = None;
    for (start, finish) in intervals {
        let start_date = timestamp(start)?;
        let finish_date = timestamp(finish)?;
        if finish_date < start_date {
            return Err("collection ends before it starts".into());
        }
        if earliest.as_ref().is_none_or(|(date, _)| start_date < *date) {
            earliest = Some((start_date, start));
        }
        if latest.as_ref().is_none_or(|(date, _)| finish_date > *date) {
            latest = Some((finish_date, finish));
        }
    }
    earliest
        .zip(latest)
        .map(|((_, start), (_, finish))| (start.clone(), finish.clone()))
        .ok_or_else(|| "empty collection interval".into())
}

/// Build the existing scientific observation from native graph/category results.
/// No measure is inferred from length, current bot flags or missing evidence.
pub fn observation_row(
    page: &serde_json::Value,
    choice: &CategoryChoice,
    incoming: u64,
    outgoing: u64,
) -> Result<serde_json::Value, String> {
    use serde_json::json;
    if page["page_id"].as_i64().is_none()
        || !page["title"].is_string()
        || choice
            .1
            .iter()
            .any(|(_, score, _, priority)| !score.is_finite() || !priority.is_finite())
    {
        return Err("Identità della pagina o ranking delle categorie non validi".into());
    }
    let created = &page["page_created_at"];
    let candidates = choice.1.iter().map(|(category,score,support,priority)|json!({"category":category,"score":score,"support":support,"priority":priority})).collect::<Vec<_>>();
    Ok(
        json!({"page_id":page["page_id"],"title":page["title"],"revision_id":page["revision_id"],
        "wikibase_item":page["wikibase_item"],"category":choice.0,"category_candidates":candidates,
        "category_status":if choice.2 {"reviewed"} else if choice.0.is_some() {"heuristic_review_needed"} else {"no_eligible_category"},
        "category_reason":if choice.2 {"Scelta manuale verificata"} else {"Categorie visibili; esclusioni configurate; rimozione dei sovratemi; specificità e numerosità. Non è una classificazione semantica validata."},
        "categories":page["categories"],"created_at":created,
        "length_bytes":page["length_bytes"],"first_public_revision_at":page["first_public_revision_at"],
        "page_created_at":page["page_created_at"],"creation_date_source":page["creation_date_source"],
        "creation_date_status":page["creation_date_status"],
        "in_degree":incoming,"out_degree":outgoing,"orphan":incoming==0}),
    )
}

/// Initialize only unsupported creator measures. Historical attribution remains
/// separate from observed creation and current census aggregates.
pub fn initialize_creator_rows(
    rows: &mut [serde_json::Value],
) -> Result<serde_json::Value, String> {
    use serde_json::{json, Value};
    for row in rows.iter_mut() {
        #[cfg(feature = "cli")]
        crate::runtime::check_cancelled()?;
        let row = row.as_object_mut().ok_or("Osservazione non valida")?;
        for field in [
            "creator_key",
            "creator_attribution",
            "creator_account_type",
            "creator_is_bot_now",
            "creator_account_type_at_creation",
            "creator_registered_at",
            "creator_tenure_days",
            "creator_tenure_status",
            "creator_prior_edits_main",
            "creator_prior_edits_other",
            "creator_prior_articles",
            "creator_articles_created_total",
            "creator_languages_created",
            "creator_languages_before",
            "origin",
            "creation_tags",
            "import_log_observed",
            "move_log_observed",
            "creator_experience_status",
            "creator_orphans_at_30_days",
            "creator_articles_in_census",
            "creator_orphans_current",
            "creator_orphans_current_share",
        ] {
            row.insert(field.into(), Value::Null);
        }
        row.insert("origin".into(), json!("unknown"));
        row.insert("creator_data_status".into(), json!("not_collected"));
        row.insert(
            "creator_orphans_at_30_days_status".into(),
            json!("unavailable_no_historical_link_graph"),
        );
    }
    Ok(
        json!({"status":"not_collected","languages_scanned":[],"matched_pages":0,"unmatched_pages":rows.len(),"orphan_30d_status":"unavailable_no_historical_link_graph"}),
    )
}

/// Join a verified History mapping to one observation batch. The caller checks
/// supplement source/release and census identities before supplying records.
pub fn join_history_rows(
    rows: &mut [serde_json::Value],
    records: &HashMap<i64, serde_json::Value>,
    missing_status: &str,
) -> Result<usize, String> {
    use serde_json::{json, Value};
    if records
        .iter()
        .any(|(id, record)| record["page_id"].as_i64() != Some(*id))
    {
        return Err("Identità della cronologia non corrispondente".into());
    }
    let mut matched = 0;
    for row in rows {
        #[cfg(feature = "cli")]
        crate::runtime::check_cancelled()?;
        let id = row["page_id"]
            .as_i64()
            .ok_or("ID dell'osservazione non valido")?;
        let row = row.as_object_mut().ok_or("Osservazione non valida")?;
        for field in [
            "history_first_revision_at",
            "history_creator_key",
            "history_creator_status",
            "creator_groups_at_creation_history",
            "creator_continuity_months_before_creation",
            "creator_continuity_bucket_before_creation",
            "creator_prior_edits_main_history",
            "creator_prior_edits_other_history",
        ] {
            row.insert(field.into(), Value::Null);
        }
        row.insert("history_data_status".into(), json!(missing_status));
        let Some(record) = records.get(&id) else {
            continue;
        };
        if let Some(created) = record.get("page_created_at") {
            row.insert("page_created_at".into(), created.clone());
            row.insert("created_at".into(), created.clone());
            row.insert(
                "creation_date_source".into(),
                json!("MediaWiki History / page_creation_timestamp"),
            );
            row.insert(
                "creation_date_status".into(),
                record
                    .get("creation_date_status")
                    .cloned()
                    .unwrap_or(json!("missing_history_creation")),
            );
        }
        let historical_date = &record["article_created_at"];
        if historical_date.is_null() || historical_date == "" {
            row.insert(
                "history_data_status".into(),
                json!("history_initial_revision_unknown"),
            );
            continue;
        }
        if !historical_date.is_string() {
            return Err("Data della prima revisione non valida".into());
        }
        row.insert("history_first_revision_at".into(), historical_date.clone());
        row.insert(
            "history_creator_status".into(),
            record["creator_attribution"].clone(),
        );
        if record["creator_history_status"] != "observed_surviving_history" {
            row.insert("history_data_status".into(), json!("unattributed_history"));
            continue;
        }
        for (target, source) in [
            ("history_creator_key", "creator_key"),
            ("creator_registered_at", "creator_registered_at"),
            ("creator_tenure_days", "creator_tenure_days"),
            ("creator_prior_articles", "creator_prior_articles"),
            ("creator_account_type", "creator_account_type"),
            ("creator_is_bot_now", "creator_is_bot_now"),
            (
                "creator_groups_at_creation_history",
                "creator_groups_at_creation",
            ),
            (
                "creator_continuity_months_before_creation",
                "creator_continuity_months_before_creation",
            ),
            (
                "creator_continuity_bucket_before_creation",
                "creator_continuity_bucket_before_creation",
            ),
            (
                "creator_prior_edits_main_history",
                "creator_prior_edits_main_history",
            ),
            (
                "creator_prior_edits_other_history",
                "creator_prior_edits_other_history",
            ),
        ] {
            row.insert(target.into(), record[source].clone());
        }
        row.insert(
            "creator_tenure_status".into(),
            json!(if record["creator_tenure_days"].is_null() {
                "missing_or_inconsistent_registration"
            } else {
                "local_registration_to_reconstructed_creation"
            }),
        );
        row.insert(
            "history_data_status".into(),
            json!("matched_reconstructed_history"),
        );
        matched += 1;
    }
    Ok(matched)
}

/// Join verified structural-quality records without substituting missing text
/// or rejecting the selected official XML revision when it differs from SQL.
pub fn join_quality_rows(
    rows: &mut [serde_json::Value],
    records: &HashMap<i64, serde_json::Value>,
) -> Result<(), String> {
    for row in rows {
        #[cfg(feature = "cli")]
        crate::runtime::check_cancelled()?;
        let id = row["page_id"]
            .as_i64()
            .ok_or("ID dell'osservazione non valido")?;
        let record = records.get(&id).ok_or("Qualità mancante per la voce")?;
        if record["page_id"].as_i64() != Some(id) || record["revision_id"] != row["revision_id"] {
            return Err("Qualità mancante o revisione diversa".into());
        }
        let status = record["quality_status"]
            .as_str()
            .ok_or("Stato della qualità mancante")?;
        let relation = record["quality_revision_relation"]
            .as_str()
            .ok_or("Relazione delle revisioni mancante")?;
        let content = &record["content_revision_id"];
        let score = &record["quality_score"];
        if !matches!(
            status,
            "scored" | "missing_current_page" | "missing_wikitext"
        ) || !matches!(relation, "same" | "different" | "unavailable")
            || (relation == "unavailable") != content.is_null()
            || (!content.is_null() && content.as_u64().is_none_or(|id| id == 0))
            || (relation == "same" && content != &row["revision_id"])
            || (relation == "different" && content == &row["revision_id"])
            || (status == "missing_current_page") != (relation == "unavailable")
            || (status == "scored"
                && !score
                    .as_f64()
                    .is_some_and(|s| (0.0..=0.998001).contains(&s)))
            || (status != "scored" && !score.is_null())
        {
            return Err("Punteggio qualità non valido".into());
        }
        let row = row.as_object_mut().ok_or("Osservazione non valida")?;
        for field in [
            "quality_score",
            "quality_status",
            "content_revision_id",
            "quality_revision_relation",
        ] {
            row.insert(field.into(), record[field].clone());
        }
    }
    Ok(())
}

/// Join complete topic records after the caller verifies the graph fingerprint
/// and approved model. Empty predictions are known negatives, never missing.
pub fn join_topic_rows(
    rows: &mut [serde_json::Value],
    records: &HashMap<i64, serde_json::Value>,
    taxonomy: &HashSet<String>,
) -> Result<usize, String> {
    if taxonomy.len() != 64 || taxonomy.iter().any(|label| !label.contains('.')) {
        return Err("Tassonomia dei temi non valida".into());
    }
    let mut predicted = 0;
    for row in rows {
        #[cfg(feature = "cli")]
        crate::runtime::check_cancelled()?;
        let id = row["page_id"]
            .as_i64()
            .ok_or("ID dell'osservazione non valido")?;
        let record = records.get(&id).ok_or("Temi mancanti per la voce")?;
        if record["page_id"].as_i64() != Some(id) || record["revision_id"] != row["revision_id"] {
            return Err("Temi o revisione non corrispondenti".into());
        }
        predicted += usize::from(validate_topic_record(record, taxonomy)?);
        let row = row.as_object_mut().ok_or("Osservazione non valida")?;
        for field in ["topic_labels", "topic_status", "topic_input_count"] {
            row.insert(field.into(), record[field].clone());
        }
    }
    Ok(predicted)
}

pub(crate) fn validate_topic_record(
    record: &serde_json::Value,
    taxonomy: &HashSet<String>,
) -> Result<bool, String> {
    let status = record["topic_status"]
        .as_str()
        .ok_or("Stato dei temi mancante")?;
    let count = record["topic_input_count"]
        .as_u64()
        .ok_or("Numero di collegamenti del modello incoerente")?;
    let labels = &record["topic_labels"];
    if !matches!(
        status,
        "predicted" | "no_model_vocabulary" | "no_mapped_outlinks"
    ) || (status == "predicted") != (count > 0)
        || (status == "predicted") == labels.is_null()
    {
        return Err("Stato di disponibilità dei temi incoerente".into());
    }
    if !labels.is_null() {
        let labels = labels.as_array().ok_or("Etichette dei temi non valide")?;
        let mut distinct = HashSet::with_capacity(labels.len());
        for label in labels {
            let label = label.as_str().ok_or("Etichetta dei temi non valida")?;
            if !taxonomy.contains(label) || !distinct.insert(label) {
                return Err("Temi sconosciuti o duplicati".into());
            }
        }
    }
    Ok(status == "predicted")
}

/// Compute both ages independently. A reconstructed or observed creation event
/// never falls back to the historical first revision when its date is missing.
pub fn observation_ages(rows: &mut [serde_json::Value], reference: &str) -> Result<(), String> {
    let collect_dates = |field: &str| {
        rows.iter()
            .map(|row| match &row[field] {
                serde_json::Value::Null => Ok(None),
                serde_json::Value::String(date) => Ok(Some(date.clone())),
                _ => Err("Data dell'osservazione non valida".to_owned()),
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let dates = collect_dates("page_created_at")?;
    let history_dates = collect_dates("history_first_revision_at")?;
    let ages = article_ages(reference, &dates)?;
    let historical = article_ages(reference, &history_dates)?;
    for (((row, date), (age, after)), (historical_age, _)) in
        rows.iter_mut().zip(dates).zip(ages).zip(historical)
    {
        let row = row.as_object_mut().ok_or("Osservazione non valida")?;
        row.insert("created_at".into(), serde_json::json!(date));
        row.insert("age_days".into(), serde_json::json!(age));
        row.insert(
            "history_first_revision_age_days".into(),
            serde_json::json!(historical_age),
        );
        if after {
            row.insert(
                "creation_date_status".into(),
                serde_json::json!("after_graph_collection"),
            );
        }
    }
    Ok(())
}

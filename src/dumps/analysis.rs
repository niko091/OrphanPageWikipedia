use super::DumpResult;
#[cfg(feature = "cli")]
use super::{
    canonical_json_sha256, file_sha256, file_signature, sql_category_parents, sql_redirects,
    stage_page_batches, stage_supplement, supplement_metadata, ImportedGraph, RecordSpool,
    StagedRecords, SupplementKind, HISTORY_RULE,
};

#[cfg(feature = "cli")]
pub(super) fn analysis_rules(rules: &serde_json::Value) -> DumpResult<crate::CategoryRules> {
    use serde_json::json;
    if !rules.is_object() {
        return Err("Regole delle categorie non valide".into());
    }
    Ok(serde_json::from_value(json!([
        rules.get("exclude_patterns").cloned().unwrap_or(json!([])),
        rules
            .get("exclude_categories")
            .cloned()
            .unwrap_or(json!([])),
        rules.get("category_priority").cloned().unwrap_or(json!({})),
        rules.get("page_overrides").cloned().unwrap_or(json!({})),
        rules.get("min_support").cloned().unwrap_or(json!(5))
    ]))?)
}

#[cfg(feature = "cli")]
pub(super) fn supplement_records(
    supplement: &serde_json::Value,
) -> DumpResult<std::collections::HashMap<i64, serde_json::Value>> {
    let mut records = std::collections::HashMap::new();
    for record in supplement["pages"]
        .as_array()
        .ok_or("Pagine del supplemento mancanti")?
    {
        let id = record["page_id"]
            .as_i64()
            .ok_or("ID del supplemento non valido")?;
        if records.insert(id, record.clone()).is_some() {
            return Err("ID duplicato nel supplemento".into());
        }
    }
    Ok(records)
}

/// Analyze a complete small or synthetic census with the same scientific rows
/// as the bounded SQL path. This preserves the legacy canonical graph checksum
/// and the separate metadata of the in-memory path; size selects execution only.
#[cfg(feature = "cli")]
pub fn analyze_memory_graph(
    raw: &serde_json::Value,
    rules: &serde_json::Value,
    history: Option<&serde_json::Value>,
    quality: Option<&serde_json::Value>,
    topics: Option<&serde_json::Value>,
    include_sources: bool,
) -> DumpResult<serde_json::Value> {
    crate::runtime::phase("analysis.memory", || {
        crate::runtime::check_cancelled()?;
        use serde_json::json;
        use std::collections::{HashMap, HashSet};
        let mut meta = raw["metadata"].clone();
        let language = meta["language"].as_str().ok_or("Lingua mancante")?;
        let database = crate::wiki_database(language, meta["database"].as_str())?;
        let pages = raw["pages"].as_array().ok_or("Pagine mancanti")?;
        if meta["complete"] != true
            || meta.get("biographies").is_some()
            || pages.iter().any(|p| p.get("biography").is_some())
        {
            return Err("Serve un censimento completo senza arricchimenti esterni".into());
        }
        let mut ids = HashSet::new();
        let mut titles = Vec::with_capacity(pages.len());
        let mut links = Vec::with_capacity(pages.len());
        let mut labels = Vec::with_capacity(pages.len());
        for page in pages {
            crate::runtime::check_cancelled()?;
            let id = page["page_id"].as_i64().ok_or("ID pagina non valido")?;
            if !ids.insert(id) {
                return Err("ID pagina duplicato".into());
            }
            titles.push(page["title"].as_str().ok_or("Titolo mancante")?.to_owned());
            links.push(serde_json::from_value(
                page.get("links").cloned().unwrap_or(json!([])),
            )?);
            labels.push((
                id,
                serde_json::from_value(page.get("categories").cloned().unwrap_or(json!([])))?,
            ));
        }
        let redirects: HashMap<String, String> =
            serde_json::from_value(raw.get("redirects").cloned().unwrap_or(json!({})))?;
        let (incoming, outgoing, unresolved, sources) = if include_sources {
            let (a, b, c, d) = crate::graph_details(titles, links, redirects)?;
            (a, b, c, Some(d))
        } else {
            let (a, b, c) = crate::graph_counts(titles, links, redirects)?;
            (a, b, c, None)
        };
        let choices = crate::classify_categories(
            &labels,
            &serde_json::from_value(raw.get("category_parents").cloned().unwrap_or(json!({})))?,
            analysis_rules(rules)?,
        )?;
        let mut rows = pages
            .iter()
            .zip(choices)
            .zip(incoming)
            .zip(outgoing)
            .map(|(((p, c), i), o)| {
                crate::runtime::check_cancelled()?;
                crate::observation_row(p, &c, i, o)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let creators = crate::initialize_creator_rows(&mut rows)?;
        let history_meta = if let Some(supplement) = history {
            let m = &supplement["metadata"];
            if m["language"] != meta["language"]
                || m["database"] != database
                || m["source"] != format!("MediaWiki History dumps / {database}")
                || m["complete"] != true
                || !m["history_release"].is_string()
                || (meta["source"] == format!("Wikimedia SQL dumps / {database}")
                    && m["sql_release"] != meta["sql_release"])
            {
                return Err("Cronologia non corrispondente al grafo".into());
            }
            let matched = crate::join_history_rows(
                &mut rows,
                &supplement_records(supplement)?,
                "missing_page",
            )?;
            let mut m = m.clone();
            m["status"] = json!("collected");
            m["matched_pages"] = json!(matched);
            m["unmatched_pages"] = json!(rows.len() - matched);
            m["input_sha256"] = json!(canonical_json_sha256(supplement)?);
            m
        } else {
            crate::join_history_rows(&mut rows, &HashMap::new(), "not_collected")?;
            json!({"status":"not_collected","matched_pages":0})
        };
        if let Some(supplement) = quality {
            let mut m = supplement_metadata(
                SupplementKind::Quality,
                supplement["metadata"].clone(),
                &meta,
                rows.len() as u64,
                None,
            )?;
            let records = supplement_records(supplement)?;
            if records.len() != ids.len() || records.keys().any(|id| !ids.contains(id)) {
                return Err("Censimento qualità incompleto".into());
            }
            crate::join_quality_rows(&mut rows, &records)?;
            for (field, value, key) in [
                ("quality_status", "scored", "scored_pages"),
                ("quality_revision_relation", "same", "same_revision_pages"),
                (
                    "quality_revision_relation",
                    "different",
                    "different_revision_pages",
                ),
                (
                    "quality_revision_relation",
                    "unavailable",
                    "unavailable_revision_pages",
                ),
            ] {
                if m[key].as_u64() != Some(rows.iter().filter(|r| r[field] == value).count() as u64)
                {
                    return Err("Copertura qualità incoerente".into());
                }
            }
            m["input_sha256"] = json!(canonical_json_sha256(supplement)?);
            meta["quality"] = m;
        }
        let reconstructed = history_meta["creation_rule"] == HISTORY_RULE;
        crate::observation_ages(
            &mut rows,
            meta["finished_at"]
                .as_str()
                .ok_or("Fine del censimento mancante")?,
        )?;
        let raw_checksum = canonical_json_sha256(raw)?;
        if let Some(supplement) = topics {
            let mut m = supplement["metadata"].clone();
            let demo = meta["demo"].as_bool().unwrap_or(false);
            let labels = m["model"]["labels"]
                .as_array()
                .ok_or("Tassonomia mancante")?;
            let taxonomy = labels
                .iter()
                .map(|l| {
                    l.as_str()
                        .filter(|s| s.contains('.'))
                        .map(str::to_owned)
                        .ok_or("Etichetta tema non valida")
                })
                .collect::<Result<HashSet<_>, _>>()?;
            if m["language"] != meta["language"]
                || m["complete"] != true
                || m["source"]
                    != if demo {
                        "local_model_on_synthetic_outlinks"
                    } else {
                        "local_model_on_verified_SQL_dump_outlinks"
                    }
                || m["demo"].as_bool().unwrap_or(false) != demo
                || m["input_sha256"] != raw_checksum
                || labels.len() != 64
                || taxonomy.len() != 64
                || m["model"]["threshold"] != 0.5
            {
                return Err("Temi non corrispondenti al grafo completo".into());
            }
            let records = supplement_records(supplement)?;
            if records.len() != ids.len() || records.keys().any(|id| !ids.contains(id)) {
                return Err("Censimento dei temi incompleto".into());
            }
            crate::join_topic_rows(&mut rows, &records, &taxonomy)?;
            m["status"] = json!("predicted");
            m["input_checksum_verified"] = json!(true);
            meta["topics"] = m;
        }
        for (key, value) in [
            ("creators", creators),
            ("history", history_meta),
            ("unresolved_target_occurrences", json!(unresolved)),
            ("category_rule", json!("structural_specificity_v1")),
            ("category_rules", rules.clone()),
            ("engine", json!("rust")),
            ("measures_rule", json!("creation_quality_creators_v3")),
            (
                "age_rule",
                json!(if reconstructed {
                    HISTORY_RULE
                } else {
                    "unavailable_no_approved_creation_event"
                }),
            ),
            (
                "historical_age_rule",
                json!(if history.is_some() {
                    "mediawiki_history_first_observed_revision_v1"
                } else {
                    "not_collected"
                }),
            ),
            ("length_unit", json!("UTF-8 wikitext bytes")),
            ("input_sha256", json!(raw_checksum)),
        ] {
            meta[key] = value;
        }
        for (enabled, key, value) in [
            (quality.is_some(), "quality", "johnson_v2_2022_01"),
            (topics.is_some(), "topics", "local_model_predictions"),
            (
                reconstructed,
                "age",
                "MediaWiki History / page_creation_timestamp",
            ),
        ] {
            if enabled {
                if meta["article_features"].is_null() {
                    meta["article_features"] = json!({});
                }
                meta["article_features"]
                    .as_object_mut()
                    .ok_or("Misure non valide")?
                    .insert(key.into(), json!(value));
            }
        }
        let mut result = json!({"metadata":meta,"pages":rows});
        if let Some(sources) = sources {
            result["_incoming_sources"] = json!(sources);
        }
        Ok(result)
    })
}

/// Inputs for the bounded canonical SQL analysis. This API prepares one member;
/// CSV, browser transports and manifest publication belong to the export stage.
#[cfg(feature = "cli")]
pub struct AnalysisOptions<'a> {
    pub rules: &'a serde_json::Value,
    pub work_dir: Option<&'a std::path::Path>,
    pub history: Option<&'a std::path::Path>,
    pub quality: Option<&'a std::path::Path>,
    pub topics: Option<&'a std::path::Path>,
    pub batch_size: usize,
}

/// Analyze the complete verified SQL census, retaining only title/redirect and
/// category indices, degree arrays and bounded observation/supplement batches.
/// A failed census or join cannot replace an approved canonical output.
#[cfg(feature = "cli")]
pub fn analyze_imported_graph(
    raw_path: &std::path::Path,
    output: &std::path::Path,
    options: &AnalysisOptions<'_>,
) -> DumpResult<serde_json::Value> {
    crate::runtime::phase("analysis.sql", || {
        crate::runtime::check_cancelled()?;
        use serde_json::{json, Value};
        use std::{collections::HashMap, io::Write};
        if !(1..=512).contains(&options.batch_size) || !options.rules.is_object() {
            return Err("Regole o dimensione del blocco di analisi non valide".into());
        }
        let imported = ImportedGraph::open(raw_path, options.work_dir, false)?;
        let supplements = [options.history, options.quality, options.topics]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        imported.check_output(raw_path, output, &supplements)?;
        let ImportedGraph {
            metadata: graph_metadata,
            manifest_path,
            source,
            expected,
            raw_signature,
            ..
        } = imported;
        let rules = options.rules;
        let native_rules = analysis_rules(rules)?;
        let mut ids = Vec::with_capacity(expected);
        let mut titles = Vec::with_capacity(expected);
        {
            let mut query = source.prepare("SELECT d.id,d.title,p.data FROM dump_page d LEFT JOIN pages p ON p.id=d.id WHERE d.ns=0 AND d.redirect=0 ORDER BY d.id")?;
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? {
                let id = row.get::<_, i64>(0)?;
                let title = row.get::<_, String>(1)?;
                let page: Value = serde_json::from_str(&row.get::<_, String>(2)?)?;
                if page["page_id"].as_i64() != Some(id)
                    || page["title"] != title
                    || ids.last().is_some_and(|last| *last >= id)
                {
                    return Err("Identità delle pagine SQL incomplete o incoerenti".into());
                }
                ids.push(id);
                titles.push(title);
            }
        }
        if ids.len() != expected {
            return Err("Identità SQL incomplete".into());
        }
        let mut graph = crate::GraphAccumulator::new(titles, sql_redirects(&source)?, None)?;
        let mut categories = crate::CategoryBatchClassifier::new(
            expected,
            sql_category_parents(&source)?.into_iter().collect(),
            native_rules,
        )?;
        let mut position = 0;
        stage_page_batches(&source, options.batch_size, true, |batch| {
            let mut targets = Vec::with_capacity(batch.len());
            let mut labels = Vec::with_capacity(batch.len());
            for page in batch {
                if ids.get(position) != Some(&page.id) {
                    return Err("Ordine delle pagine SQL incoerente".into());
                }
                targets.push(page.links);
                labels.push(page.categories);
                position += 1;
            }
            graph.add_batch(targets)?;
            categories.add_support_batch(labels)?;
            Ok(())
        })?;
        if position != expected {
            return Err("Analisi SQL non completa".into());
        }
        let (incoming, outgoing, unresolved) = graph.finish()?;
        drop(graph);
        let raw_checksum = file_sha256(raw_path)?;
        // An empty SQLite filename is a private disk-backed temporary database,
        // deleted by SQLite when the owned connection closes, including on errors.
        let mut stage = rusqlite::Connection::open("")?;
        stage.execute_batch("PRAGMA cache_size=-8192; PRAGMA temp_store=FILE")?;
        let mut history_meta = json!({"status":"not_collected","matched_pages":0});
        let mut quality_meta = None;
        let mut topic_meta = None;
        for (path, kind) in [
            (options.history, SupplementKind::History),
            (options.quality, SupplementKind::Quality),
            (options.topics, SupplementKind::Topics),
        ] {
            if let Some(path) = path {
                let meta = stage_supplement(
                    path,
                    &mut stage,
                    kind,
                    &graph_metadata,
                    expected,
                    Some(&raw_checksum),
                )?;
                match kind {
                    SupplementKind::History => history_meta = meta,
                    SupplementKind::Quality => quality_meta = Some(meta),
                    SupplementKind::Topics => topic_meta = Some(meta),
                }
            }
        }
        stage.execute_batch("CREATE TABLE IF NOT EXISTS history(id INTEGER PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS quality(id INTEGER PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS topics(id INTEGER PRIMARY KEY,data TEXT NOT NULL)")?;
        let mut history_query = stage.prepare("SELECT id,data FROM history ORDER BY id")?;
        let mut quality_query = stage.prepare("SELECT id,data FROM quality ORDER BY id")?;
        let mut topic_query = stage.prepare("SELECT id,data FROM topics ORDER BY id")?;
        let mut history = StagedRecords::new(&mut history_query)?;
        let mut quality = StagedRecords::new(&mut quality_query)?;
        let mut topics = StagedRecords::new(&mut topic_query)?;
        let taxonomy = topic_meta
            .as_ref()
            .map(|meta| serde_json::from_value(meta["model"]["labels"].clone()))
            .transpose()?
            .unwrap_or_default();
        let (spool, file) = RecordSpool::new(output)?;
        let mut writer = std::io::BufWriter::with_capacity(65536, file);
        position = 0;
        stage_page_batches(&source, options.batch_size, false, |batch| {
            let choices = categories
                .classify_batch(batch.iter().map(|p| (p.id, p.categories.clone())).collect())?;
            let mut rows = Vec::with_capacity(batch.len());
            for (mut page, choice) in batch.into_iter().zip(choices) {
                if ids.get(position) != Some(&page.id) {
                    return Err("Ordine SQL cambiato durante l'export".into());
                }
                page.data["categories"] = json!(page.categories);
                rows.push(crate::observation_row(
                    &page.data,
                    &choice,
                    incoming[position],
                    outgoing[position],
                )?);
                position += 1;
            }
            crate::initialize_creator_rows(&mut rows)?;
            for (enabled, kind, cursor) in [
                (
                    options.history.is_some(),
                    SupplementKind::History,
                    &mut history,
                ),
                (
                    options.quality.is_some(),
                    SupplementKind::Quality,
                    &mut quality,
                ),
                (
                    options.topics.is_some(),
                    SupplementKind::Topics,
                    &mut topics,
                ),
            ] {
                let mut records = HashMap::with_capacity(rows.len());
                if enabled {
                    for row in &rows {
                        let id = row["page_id"].as_i64().ok_or("ID osservazione mancante")?;
                        records.insert(id, cursor.take(id)?);
                    }
                }
                match kind {
                    SupplementKind::History => {
                        crate::join_history_rows(
                            &mut rows,
                            &records,
                            if enabled {
                                "missing_page"
                            } else {
                                "not_collected"
                            },
                        )?;
                    }
                    SupplementKind::Quality if enabled => {
                        crate::join_quality_rows(&mut rows, &records)?
                    }
                    SupplementKind::Topics if enabled => {
                        crate::join_topic_rows(&mut rows, &records, &taxonomy)?;
                    }
                    _ => {}
                }
            }
            crate::observation_ages(
                &mut rows,
                graph_metadata["finished_at"]
                    .as_str()
                    .ok_or("Data SQL mancante")?,
            )?;
            for row in rows {
                serde_json::to_writer(&mut writer, &row)?;
                writer.write_all(b"\n")?;
            }
            Ok(())
        })?;
        history.finish()?;
        quality.finish()?;
        topics.finish()?;
        if position != expected
            || file_signature(raw_path)? != raw_signature
            || file_sha256(&manifest_path)? != graph_metadata["dump_manifest_sha256"]
        {
            return Err("Censimento incompleto o input modificati durante l'analisi".into());
        }
        writer.flush()?;
        drop(writer);
        let mut meta = graph_metadata;
        meta["creators"] = crate::initialize_creator_rows(&mut [])?;
        meta["history"] = history_meta;
        for (key, value) in [
            ("unresolved_target_occurrences", json!(unresolved)),
            ("category_rule", json!("structural_specificity_v1")),
            ("category_rules", rules.clone()),
            ("engine", json!("rust")),
            ("age_rule", json!("unavailable_no_approved_creation_event")),
            ("measures_rule", json!("creation_quality_creators_v3")),
            (
                "historical_age_rule",
                json!(if options.history.is_some() {
                    "mediawiki_history_first_observed_revision_v1"
                } else {
                    "not_collected"
                }),
            ),
            ("length_unit", json!("UTF-8 wikitext bytes")),
            ("input_sha256", json!(raw_checksum)),
            ("input_checksum_format", json!("raw_file_bytes_v1")),
            ("analysis_source", json!("verified_import_stage")),
        ] {
            meta[key] = value;
        }
        if meta["history"]["creation_rule"] == HISTORY_RULE {
            meta["age_rule"] = json!(HISTORY_RULE);
            if meta["article_features"].is_null() {
                meta["article_features"] = json!({});
            }
            meta["article_features"]
                .as_object_mut()
                .ok_or("Metadati delle misure non validi")?
                .insert(
                    "age".into(),
                    json!("MediaWiki History / page_creation_timestamp"),
                );
        }
        for (key, supplement, feature) in [
            ("quality", quality_meta, "johnson_v2_2022_01"),
            ("topics", topic_meta, "local_model_predictions"),
        ] {
            if let Some(supplement) = supplement {
                meta[key] = supplement;
                if meta["article_features"].is_null() {
                    meta["article_features"] = json!({});
                }
                let features = meta["article_features"]
                    .as_object_mut()
                    .ok_or("Metadati delle misure non validi")?;
                features.insert(key.into(), json!(feature));
            }
        }
        spool.publish(output, &meta)?;
        Ok(meta)
    })
}

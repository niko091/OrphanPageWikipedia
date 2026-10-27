use super::DumpResult;
#[cfg(feature = "cli")]
use super::{
    file_signature, resolved_path, visit_ordered_json, write_canonical_json, CanonicalDigest,
};

/// The table and scientific validation rules for a complete SQL supplement.
#[cfg(feature = "cli")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupplementKind {
    History,
    Quality,
    Topics,
}

#[cfg(feature = "cli")]
pub(super) fn supplement_metadata(
    kind: SupplementKind,
    mut meta: serde_json::Value,
    graph: &serde_json::Value,
    count: u64,
    raw_checksum: Option<&str>,
) -> DumpResult<serde_json::Value> {
    use serde_json::{json, Value};
    if meta["language"] != graph["language"] || meta["complete"] != true {
        return Err("Supplemento non corrispondente al censimento SQL".into());
    }
    match kind {
        SupplementKind::History => {
            if meta["database"] != graph["database"]
                || meta["sql_release"] != graph["sql_release"]
                || meta["source"]
                    != format!(
                        "MediaWiki History dumps / {}",
                        graph["database"].as_str().ok_or("Database SQL mancante")?
                    )
            {
                return Err("Cronologia non corrispondente al grafo SQL".into());
            }
            meta["status"] = json!("collected");
        }
        SupplementKind::Quality => {
            let approved: Value = serde_json::from_str(include_str!("../../config/quality.json"))?;
            let scored = meta["scored_pages"].as_u64();
            let relations = [
                "same_revision_pages",
                "different_revision_pages",
                "unavailable_revision_pages",
            ]
            .map(|key| meta[key].as_u64());
            let total = relations
                .into_iter()
                .try_fold(0u64, |sum, n| sum.checked_add(n?));
            if meta["database"] != graph["database"]
                || meta["sql_release"] != graph["sql_release"]
                || meta["source"] != "Johnson V2 / mediawiki_content_current"
                || meta["model"] != approved
                || meta["dump_manifest_sha256"] != graph["dump_manifest_sha256"]
                || meta["graph_finished_at"] != graph["finished_at"]
                || meta["census_page_count"].as_u64() != Some(count)
                || meta["quality_revision_rule"] != "selected_official_xml_revision_v1"
                || scored.is_none_or(|n| n > count)
                || meta["missing_pages"].as_u64() != scored.and_then(|n| count.checked_sub(n))
                || total != Some(count)
                || raw_checksum.is_some_and(|sha| meta["input_file_sha256"] != sha)
            {
                return Err("Qualità non corrispondente al grafo SQL completo".into());
            }
            if !meta["normalizers"].as_array().is_some_and(|values| {
                values.len() == 6
                    && values
                        .iter()
                        .all(|v| v.as_f64().is_some_and(|n| n > 0.0 && n.is_finite()))
            }) {
                return Err("Normalizzatori qualità non validi".into());
            }
            meta["status"] = json!("collected");
        }
        SupplementKind::Topics => {
            let approved: Value = serde_json::from_str(include_str!("../../config/topics.json"))?;
            if meta["source"] != "local_model_on_verified_SQL_dump_outlinks"
                || meta["graph_finished_at"] != graph["finished_at"]
                || raw_checksum.is_none_or(|sha| meta["input_file_sha256"] != sha)
                || meta["model"] != approved
            {
                return Err("Temi non corrispondenti al grafo SQL e al modello configurato".into());
            }
            meta["status"] = json!("predicted");
            meta["input_checksum_verified"] = json!(true);
        }
    }
    Ok(meta)
}

/// Index a complete supplement in a caller-owned private analysis database.
/// Keep one record and one prepared INSERT resident. A savepoint rolls back
/// the table and all records on any parse, validation, I/O or SQLite failure.
/// The caller must verify the source graph and join IDs/revisions against it;
/// counts alone cannot establish that two censuses contain the same pages.
#[cfg(feature = "cli")]
pub fn stage_supplement(
    path: &std::path::Path,
    connection: &mut rusqlite::Connection,
    kind: SupplementKind,
    graph: &serde_json::Value,
    expected_count: usize,
    raw_checksum: Option<&str>,
) -> DumpResult<serde_json::Value> {
    crate::runtime::phase("supplement.stage", || {
        crate::runtime::check_cancelled()?;
        use serde_json::{json, Value};
        use sha2::Digest;
        use std::collections::HashSet;
        use std::io::{BufReader, BufWriter, Write};
        let database = graph["database"].as_str().ok_or("Database SQL mancante")?;
        if graph["complete"] != true
            || graph["source"] != format!("Wikimedia SQL dumps / {database}")
            || graph["language"].as_str().is_none()
            || graph["sql_release"].as_str().is_none()
            || (kind != SupplementKind::History && graph["finished_at"].as_str().is_none())
        {
            return Err("Serve il censimento SQL approvato".into());
        }
        if let Some(db) = connection.path().filter(|p| !p.is_empty()) {
            if resolved_path(path)? == resolved_path(std::path::Path::new(db))? {
                return Err("Staging sovrapposto al supplemento".into());
            }
        }
        let signature = file_signature(path)?;
        let reader = BufReader::with_capacity(65536, std::fs::File::open(path)?);
        let table = match kind {
            SupplementKind::History => "history",
            SupplementKind::Quality => "quality",
            SupplementKind::Topics => "topics",
        };
        let transaction = connection.savepoint()?;
        transaction.execute_batch(&format!(
            "CREATE TABLE {table}(id INTEGER PRIMARY KEY, data TEXT NOT NULL)"
        ))?;
        let mut metadata = None;
        let mut taxonomy = HashSet::new();
        let mut count = 0usize;
        let mut observed = 0u64;
        let mut relations = [0u64; 3];
        let mut digest = CanonicalDigest::default();
        {
            let mut writer = BufWriter::with_capacity(65536, &mut digest);
            let mut insert = transaction.prepare(&format!("INSERT INTO {table} VALUES(?,?)"))?;
            visit_ordered_json(reader, false, false, &mut |section, _, record| {
                if section == "metadata" {
                    let meta = supplement_metadata(
                        kind,
                        record.clone(),
                        graph,
                        u64::try_from(expected_count)?,
                        raw_checksum,
                    )?;
                    if kind == SupplementKind::Topics {
                        taxonomy = meta["model"]["labels"]
                            .as_array()
                            .ok_or("Tassonomia mancante")?
                            .iter()
                            .map(|v| v.as_str().map(String::from).ok_or("Etichetta non valida"))
                            .collect::<Result<HashSet<_>, _>>()?;
                    } else {
                        let mut original = record;
                        if kind == SupplementKind::Quality {
                            original
                                .as_object_mut()
                                .ok_or("Metadati non validi")?
                                .remove("status");
                        }
                        writer.write_all(b"{\"metadata\": ")?;
                        write_canonical_json(&original, &mut writer)?;
                        writer.write_all(b", \"pages\": [")?;
                    }
                    metadata = Some(meta);
                    return Ok(());
                }
                let id = record["page_id"]
                    .as_i64()
                    .ok_or("ID supplemento non valido")?;
                if count >= expected_count {
                    return Err("Supplemento oltre il censimento SQL".into());
                }
                match kind {
                    SupplementKind::History => {
                        // Python's truth test is relevant only for present dates;
                        // row joins validate the scientific fields separately.
                        let date = &record["article_created_at"];
                        let present = match date {
                            Value::Null => false,
                            Value::String(s) => !s.is_empty(),
                            _ => return Err("Data di creazione History non valida".into()),
                        };
                        observed += u64::from(
                            present
                                && record["creator_history_status"] == "observed_surviving_history",
                        );
                    }
                    SupplementKind::Quality => {
                        observed += u64::from(record["quality_status"] == "scored");
                        let relation = match record["quality_revision_relation"].as_str() {
                            Some("same") => 0,
                            Some("different") => 1,
                            Some("unavailable") => 2,
                            _ => return Err("Relazione delle revisioni qualità non valida".into()),
                        };
                        relations[relation] += 1;
                    }
                    SupplementKind::Topics => {
                        observed += u64::from(crate::validate_topic_record(&record, &taxonomy)?);
                    }
                }
                if kind != SupplementKind::Topics {
                    if count != 0 {
                        writer.write_all(b", ")?;
                    }
                    write_canonical_json(&record, &mut writer)?;
                }
                insert.execute(rusqlite::params![id, serde_json::to_string(&record)?])?;
                count += 1;
                Ok(())
            })?;
            writer.write_all(b"]}")?;
            writer.flush()?;
        }
        let mut meta = metadata.ok_or("Metadati supplemento mancanti")?;
        if count != expected_count || file_signature(path)? != signature {
            return Err("Supplemento incompleto o modificato durante la lettura".into());
        }
        match kind {
            SupplementKind::History => {
                meta["matched_pages"] = json!(observed);
                meta["unmatched_pages"] = json!(u64::try_from(count)? - observed);
            }
            SupplementKind::Quality => {
                if meta["scored_pages"].as_u64() != Some(observed)
                    || [
                        "same_revision_pages",
                        "different_revision_pages",
                        "unavailable_revision_pages",
                    ]
                    .iter()
                    .zip(relations)
                    .any(|(key, n)| meta[*key].as_u64() != Some(n))
                {
                    return Err("Censimento qualità incompleto".into());
                }
            }
            SupplementKind::Topics => {
                if meta["predicted_pages"].as_u64() != Some(observed)
                    || meta["missing_pages"].as_u64() != Some(u64::try_from(count)? - observed)
                {
                    return Err("Checkpoint dei temi incompleto".into());
                }
            }
        }
        if kind != SupplementKind::Topics {
            meta["input_sha256"] = json!(format!("{:x}", digest.0.finalize()));
        }
        transaction.commit()?;
        Ok(meta)
    })
}

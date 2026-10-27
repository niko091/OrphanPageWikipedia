use super::DumpResult;
#[cfg(feature = "cli")]
use super::{
    approved_graph, available_scientific_name, comparison_filename, comparison_signature,
    dataset_filename, export_directory, file_sha256, file_signature, first_metadata,
    prepare_member, publish_prepared, stage_export, stage_supplement, write_atomic,
    CanonicalExport, DumpClient, SupplementKind, WikimediaHttp, HISTORY_RULE,
};

#[cfg(feature = "cli")]
pub(super) fn write_staged_export(
    staged: &CanonicalExport,
    output: &std::path::Path,
) -> DumpResult<()> {
    use std::io::{BufWriter, Write};
    write_atomic(output, |file| {
        let mut writer = BufWriter::with_capacity(65536, file);
        writer.write_all(b"{\"metadata\":")?;
        serde_json::to_writer(&mut writer, &staged.metadata)?;
        writer.write_all(b",\"pages\":[")?;
        let mut query = staged
            .database
            .prepare("SELECT data FROM pages ORDER BY position")?;
        let mut rows = query.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            if count > 0 {
                writer.write_all(b",")?;
            }
            writer.write_all(row.get::<_, String>(0)?.as_bytes())?;
            count += 1;
        }
        if count != staged.count {
            return Err("Censimento export incompleto".into());
        }
        writer.write_all(b"],\"link_candidates\":{")?;
        // The frozen refresh spool used TEXT keys, unlike graph matching's
        // INTEGER keys. Preserve its canonical record order as well as edges.
        let mut query = staged
            .database
            .prepare("SELECT id,data FROM candidates ORDER BY CAST(id AS TEXT)")?;
        let mut rows = query.query([])?;
        let mut first = true;
        while let Some(row) = rows.next()? {
            if !first {
                writer.write_all(b",")?;
            }
            first = false;
            serde_json::to_writer(&mut writer, &row.get::<_, i64>(0)?.to_string())?;
            writer.write_all(b":")?;
            writer.write_all(row.get::<_, String>(1)?.as_bytes())?;
        }
        writer.write_all(b"}}")?;
        writer.flush()?;
        Ok(())
    })
}

/// Join new verified History/quality measures to an approved complete export.
/// Degrees, topics, original page order and every candidate/reference edge are
/// retained on disk. No graph calculation, matching or topic inference runs.
#[cfg(feature = "cli")]
pub fn refresh_measures(
    raw: &std::path::Path,
    history: &std::path::Path,
    output: &std::path::Path,
    quality: Option<&std::path::Path>,
) -> DumpResult<String> {
    crate::runtime::phase("measures.refresh", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::params;
        use serde_json::{json, Value};
        use std::collections::HashMap;
        let graph = first_metadata(raw)?;
        approved_graph(&graph, false)?;
        let language = graph["language"].as_str().ok_or("Lingua grafo mancante")?;
        if graph["demo"] == true {
            return Err("Aggiornamento misure riservato ai dump SQL approvati".into());
        }
        let original_manifest = std::fs::read(output.join("manifest.json"))?;
        let mut manifest: Vec<Value> = serde_json::from_slice(&original_manifest)?;
        let rank = |e: &Value| {
            (
                e["date"].as_str().unwrap_or("").to_owned(),
                e["published_at"].as_str().unwrap_or("").to_owned(),
            )
        };
        let entry = manifest
            .iter()
            .filter(|e| {
                e["language"] == language
                    && e["demo"] != true
                    && e.get("kind").and_then(Value::as_str).unwrap_or("wiki") == "wiki"
            })
            .max_by_key(|e| rank(e))
            .ok_or("Nessun export approvato da aggiornare")?;
        let original = output.join(dataset_filename(&entry["file"])?);
        let mut staged = stage_export(&original)?;
        if staged.count == 0
            || staged.metadata["complete"] != true
            || staged.metadata["demo"] == true
            || staged.metadata["language"] != language
            || [
                "source",
                "dump_manifest_sha256",
                "sql_release",
                "finished_at",
            ]
            .iter()
            .any(|k| staged.metadata[*k] != graph[*k])
        {
            return Err("Export e grafo non corrispondenti; analizzare prima il grafo".into());
        }
        let inputs = [Some(raw), Some(history), quality]
            .into_iter()
            .flatten()
            .map(|p| Ok((p, comparison_signature(p)?)))
            .collect::<DumpResult<Vec<_>>>()?;
        let raw_checksum = file_sha256(raw)?;
        if staged.metadata["input_checksum_format"] == "raw_file_bytes_v1"
            && staged.metadata["input_sha256"] != raw_checksum
        {
            return Err("Checksum del grafo diverso dall'export approvato".into());
        }
        let history_meta = stage_supplement(
            history,
            &mut staged.database,
            SupplementKind::History,
            &graph,
            staged.count,
            None,
        )?;
        if history_meta["creation_rule"] != HISTORY_RULE {
            return Err("Reimportare History con la nuova definizione della creazione".into());
        }
        let quality_meta = quality
            .map(|path| {
                stage_supplement(
                    path,
                    &mut staged.database,
                    SupplementKind::Quality,
                    &graph,
                    staged.count,
                    Some(&raw_checksum),
                )
            })
            .transpose()?;
        for table in std::iter::once("history").chain(quality.is_some().then_some("quality")) {
            let foreign = staged.database.query_row(
                &format!(
                "SELECT COUNT(*) FROM {table} h LEFT JOIN pages p ON p.id=h.id WHERE p.id IS NULL"
            ),
                [],
                |r| r.get::<_, u64>(0),
            )?;
            if foreign != 0 {
                return Err("Identità del supplemento diverse dal censimento approvato".into());
            }
        }
        let mut after = -1i64;
        let mut processed = 0usize;
        loop {
            crate::runtime::check_cancelled()?;
            let batch = staged
            .database
            .prepare(
                "SELECT id,position,data FROM pages WHERE position>? ORDER BY position LIMIT 512",
            )?
            .query_map([after], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
            let Some(last) = batch.last() else { break };
            after = last.1;
            let ids = batch
                .iter()
                .map(|r| r.0.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let mut rows = batch
                .iter()
                .map(|r| serde_json::from_str::<Value>(&r.2))
                .collect::<Result<Vec<_>, _>>()?;
            crate::initialize_creator_rows(&mut rows)?;
            for (table, kind) in std::iter::once(("history", SupplementKind::History)).chain(
                quality
                    .is_some()
                    .then_some(("quality", SupplementKind::Quality)),
            ) {
                let records = staged
                    .database
                    .prepare(&format!("SELECT id,data FROM {table} WHERE id IN ({ids})"))?
                    .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                    .map(|r| {
                        let (id, data) = r?;
                        Ok((id, serde_json::from_str::<Value>(&data)?))
                    })
                    .collect::<DumpResult<HashMap<_, _>>>()?;
                match kind {
                    SupplementKind::History => {
                        crate::join_history_rows(&mut rows, &records, "missing_page")?;
                    }
                    SupplementKind::Quality => crate::join_quality_rows(&mut rows, &records)?,
                    SupplementKind::Topics => {
                        return Err("Join temi inatteso durante il refresh".into())
                    }
                }
            }
            let reference = graph["finished_at"]
                .as_str()
                .ok_or("Fine censimento mancante")?;
            let dates = |field: &str| -> DumpResult<Vec<Option<String>>> {
                Ok(rows
                    .iter()
                    .map(|r| serde_json::from_value(r[field].clone()))
                    .collect::<Result<_, _>>()?)
            };
            let ages = crate::article_ages(reference, &dates("page_created_at")?)?;
            let historical = crate::article_ages(reference, &dates("history_first_revision_at")?)?;
            let transaction = staged.database.transaction()?;
            let mut update = transaction.prepare("UPDATE pages SET data=? WHERE id=?")?;
            for ((mut row, (age, after)), (first, _)) in rows.into_iter().zip(ages).zip(historical)
            {
                row["age_days"] = json!(age);
                row["history_first_revision_age_days"] = json!(first);
                if after {
                    row["creation_date_status"] = json!("after_graph_collection");
                }
                update.execute(params![
                    serde_json::to_string(&row)?,
                    row["page_id"].as_i64().ok_or("ID pagina mancante")?
                ])?;
                processed += 1;
            }
            drop(update);
            transaction.commit()?;
        }
        if processed != staged.count {
            return Err("Aggiornamento misure incompleto".into());
        }
        let preserve_legacy = quality_meta.is_none()
            && staged.metadata["quality"].is_object()
            && staged.metadata["quality"]["quality_revision_rule"]
                != "selected_official_xml_revision_v1";
        let measures = if preserve_legacy {
            staged
                .metadata
                .get("measures_rule")
                .cloned()
                .unwrap_or(json!("creation_quality_creators_v2"))
        } else {
            json!("creation_quality_creators_v3")
        };
        staged.metadata["history"] = history_meta;
        staged.metadata["age_rule"] = json!(HISTORY_RULE);
        staged.metadata["measures_rule"] = measures;
        if staged.metadata["article_features"].is_null() {
            staged.metadata["article_features"] = json!({});
        }
        let features = staged.metadata["article_features"]
            .as_object_mut()
            .ok_or("Metadati misure non validi")?;
        features.insert(
            "age".into(),
            json!("MediaWiki History / page_creation_timestamp"),
        );
        if quality_meta.is_some() {
            features.insert("quality".into(), json!("johnson_v2_2022_01"));
        }
        if let Some(meta) = quality_meta {
            staged.metadata["quality"] = meta;
        }
        let references = staged.metadata["comparison"]["reference_languages"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let name = comparison_filename(language, &staged.metadata, &references)?;
        let (prepared, generation) =
            export_directory(output.parent().ok_or("Directory dati non valida")?)?;
        write_staged_export(&staged, &prepared.path.join(&name))?;
        let name = available_scientific_name(&prepared, output, &name)?;
        let member = prepare_member(&prepared, &output.join(&name), &generation)?;
        manifest.retain(|e| e["file"] != name);
        manifest.push(member.entry);
        for (path, signature) in inputs {
            if comparison_signature(path)? != signature {
                return Err("Input modificato durante l'aggiornamento".into());
            }
        }
        if file_signature(&original)? != staged.signature {
            return Err("Export modificato durante l'aggiornamento".into());
        }
        DumpClient::<WikimediaHttp>::write_json(
            &prepared.path.join("manifest.json"),
            &json!(manifest),
        )?;
        publish_prepared(
            &prepared.path,
            output,
            Some(&original_manifest),
            &member.reused,
            |from, to| std::fs::rename(from, to),
        )?;
        Ok(name)
    })
}

use super::DumpResult;
#[cfg(feature = "cli")]
use super::{
    add_article_preview, approved_graph, canonical_json_sha256, dataset_filename,
    export_comparison_member, export_directory, file_sha256, file_signature, first_metadata,
    manifest_entry, prepare_member, publish_prepared, resolved_path, reuse_export,
    visit_ordered_json, write_atomic, write_sorted_json, write_summary_files, CanonicalDigest,
    DumpClient, PreparedDirectory, ReusedExport, WikimediaHttp,
};

/// Describe complete independent edition censuses without duplicating pages.
/// Each member retains its interval, source metadata and page denominator.
#[cfg(feature = "cli")]
pub fn aggregate_index(members: &[serde_json::Value]) -> DumpResult<serde_json::Value> {
    use serde_json::{json, Value};
    if members.is_empty() {
        return Err("Aggregato senza membri".into());
    }
    let mut ordered = members.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| a["language"].as_str().cmp(&b["language"].as_str()));
    let mut census = Vec::with_capacity(members.len());
    let mut intervals = Vec::with_capacity(members.len());
    let mut languages = Vec::new();
    let demo = ordered[0]["metadata"]["demo"] == true;
    for member in ordered {
        let meta = &member["metadata"];
        let language = member["language"]
            .as_str()
            .ok_or("Lingua membro mancante")?;
        crate::wiki_database(language, meta["database"].as_str())?;
        dataset_filename(&member["file"])?;
        if languages.last().is_some_and(|last| last == language)
            || meta["language"] != language
            || meta["complete"] != true
            || (meta["demo"] == true) != demo
            || member["page_count"].as_u64().is_none()
        {
            return Err(
                "L'aggregato richiede un solo censimento completo per lingua e fonti omogenee"
                    .into(),
            );
        }
        intervals.push((
            meta["started_at"]
                .as_str()
                .ok_or("Inizio censimento mancante")?
                .to_owned(),
            meta["finished_at"]
                .as_str()
                .ok_or("Fine censimento mancante")?
                .to_owned(),
        ));
        languages.push(language.to_owned());
        census.push(json!({"language":language,"file":member["file"],"page_count":member["page_count"],"metadata":meta}));
    }
    let (start, finish) = crate::collection_interval(&intervals)?;
    let common = |key: &str| -> Value {
        let value = &census[0]["metadata"][key];
        if census.iter().all(|m| m["metadata"][key] == *value) {
            value.clone()
        } else {
            json!("mixed_per_language")
        }
    };
    Ok(
        json!({"metadata":{"kind":"aggregate","language":"all","languages":languages,"complete":true,"demo":demo,
        "source":if demo {"Synthetic complete graphs"}else{"Wikimedia SQL dumps (aggregate of source-specific releases)"},
        "started_at":start,"finished_at":finish,"temporal_consistency":"independent_source_specific_intervals","observation_unit":"language_page",
        "age_rule":common("age_rule"),"historical_age_rule":common("historical_age_rule"),"measures_rule":common("measures_rule"),"length_unit":"UTF-8 wikitext bytes"},"members":census}),
    )
}

/// Metadata shared by the prepared aggregate summaries and relational validation.
#[cfg(feature = "cli")]
pub(crate) fn aggregate_summary_metadata(
    members: &[serde_json::Value],
    metadata: &serde_json::Value,
    aggregate: bool,
) -> DumpResult<(serde_json::Value, bool)> {
    use serde_json::{json, Value};
    if members.is_empty() || !metadata.is_object() {
        return Err("Riepilogo aggregato senza membri".into());
    }
    let mut meta = metadata.clone();
    let collected =
        |value: &Value| matches!(value.as_str(), Some("collected" | "partially_collected"));
    let creators = members
        .iter()
        .filter(|m| collected(&m["metadata"]["creators"]["status"]))
        .collect::<Vec<_>>();
    let history = members
        .iter()
        .filter(|m| collected(&m["metadata"]["history"]["status"]))
        .collect::<Vec<_>>();
    let status = |count: usize| {
        if count == members.len() {
            "collected"
        } else if count == 0 {
            "not_collected"
        } else {
            "partially_collected"
        }
    };
    if aggregate {
        meta["creators"] = json!({"status":status(creators.len()),"languages_scanned":creators.iter().map(|m|&m["language"]).collect::<Vec<_>>(),"collected_languages":creators.len()});
        let matched = history
            .iter()
            .try_fold(0u64, |total, m| {
                total.checked_add(
                    m["metadata"]["history"]
                        .get("matched_pages")
                        .map(|v| v.as_u64())
                        .unwrap_or(Some(0))?,
                )
            })
            .ok_or("Conteggio History non valido")?;
        meta["history"] = json!({"status":status(history.len()),"matched_pages":matched});
    }
    Ok((meta, creators.is_empty() && !history.is_empty()))
}

#[cfg(feature = "cli")]
/// Prepare an aggregate summary and its separate creator index from approved
/// canonical member page streams. Paths are operational inputs only and never
/// enter the aggregate metadata or source fingerprints. Candidate evidence is
/// not read a second time to calculate charts.
pub fn write_aggregate_summary(
    members: &[serde_json::Value],
    output: &std::path::Path,
    metadata: &serde_json::Value,
    aggregate: bool,
) -> DumpResult<()> {
    crate::runtime::phase("dashboard.aggregate", || {
        crate::runtime::check_cancelled()?;
        use serde_json::json;
        use std::io::BufReader;
        if members.is_empty() || !metadata.is_object() {
            return Err("Riepilogo aggregato senza membri".into());
        }
        let name = output
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("Nome riepilogo mancante")?;
        let stem = name
            .strip_suffix("-summary.json")
            .ok_or("Il nome deve terminare con -summary.json")?;
        let creator_name = format!("{stem}-creators.json");
        let folder = output.parent().ok_or("Directory riepilogo mancante")?;
        let protected = [
            resolved_path(output)?,
            resolved_path(&folder.join(&creator_name))?,
        ];
        let (meta, historical) = aggregate_summary_metadata(members, metadata, aggregate)?;
        let mut summary = crate::dashboard::SummaryAccumulator::new(
            meta["language"]
                .as_str()
                .ok_or("Lingua riepilogo mancante")?
                .into(),
            aggregate,
            historical,
        );
        let mut preview = Vec::with_capacity(26);
        let mut sources = Vec::with_capacity(members.len());
        let mut expected_total = 0usize;
        for member in members {
            let language = member["language"]
                .as_str()
                .ok_or("Lingua membro mancante")?;
            crate::wiki_database(language, member["metadata"]["database"].as_str())?;
            let path =
                std::path::Path::new(member["path"].as_str().ok_or("Percorso membro mancante")?);
            if member["metadata"]["complete"] != true || member["metadata"]["language"] != language
            {
                return Err("Censimento membro non completo o incoerente".into());
            }
            if protected.contains(&resolved_path(path)?) {
                return Err("Riepilogo sovrapposto a una sorgente".into());
            }
            let signature = file_signature(path)?;
            sources.push(json!({"language":language,"file":path.file_name().and_then(|n|n.to_str()).ok_or("Nome membro non valido")?,"size":signature.0,"mtime_ns":signature.1}));
            let expected = member["page_count"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or("Denominatore membro non valido")?;
            let mut count = 0usize;
            visit_ordered_json(
                BufReader::new(std::fs::File::open(path)?),
                true,
                true,
                &mut |section, _, record| {
                    if section == "metadata" {
                        if record != member["metadata"] || record.get("biographies").is_some() {
                            return Err(
                                "Metadati del riepilogo aggregato non corrispondenti".into()
                            );
                        }
                    } else {
                        if record.get("biography").is_some() || record.get("gender_group").is_some()
                        {
                            return Err("Arricchimento non autorizzato".into());
                        }
                        let code = aggregate.then_some(language);
                        summary.add_observation(&record, code)?;
                        add_article_preview(&mut preview, &record, code)?;
                        count += 1;
                    }
                    Ok(())
                },
            )?;
            if count != expected || file_signature(path)? != signature {
                return Err(
                    "Censimento incompleto o modificato durante il riepilogo aggregato".into(),
                );
            }
            expected_total = expected_total
                .checked_add(expected)
                .ok_or("Aggregato troppo grande")?;
        }
        if summary.len() != expected_total {
            return Err("Popolazione del riepilogo aggregato incompleta".into());
        }
        let names = json!({"summary_file":name,"creator_index_file":creator_name});
        write_summary_files(
            folder,
            &names,
            &meta,
            expected_total,
            summary,
            preview.into_iter().map(|p| p.fields).collect(),
            json!(sources),
        )
    })
}

/// Stable scientific comparison names include all reference collection source
/// identities. UTF-8 sorted JSON here intentionally differs from the ASCII
/// canonical graph checksum convention, matching the original filenames.
#[cfg(feature = "cli")]
pub fn comparison_filename(
    language: &str,
    metadata: &serde_json::Value,
    languages: &[String],
) -> DumpResult<String> {
    use serde_json::json;
    use sha2::Digest;
    crate::wiki_database(language, metadata["database"].as_str())?;
    let release = metadata["sql_release"].as_str();
    let Some(release) = release.filter(|s| s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit()))
    else {
        return Ok(format!(
            "{language}{}.json",
            if metadata["demo"] == true {
                "-demo"
            } else {
                ""
            }
        ));
    };
    let references = metadata["comparison"]["collections"]
        .as_array()
        .filter(|a| !a.is_empty())
        .map(|a| json!(a))
        .unwrap_or_else(|| {
            let mut languages = languages.to_vec();
            languages.sort();
            json!(languages)
        });
    let identity = json!({"language":language,"release":release,"input":metadata["input_sha256"],"history":metadata["history"],"topics":metadata["topics"],"quality":metadata["quality"],"age_rule":metadata["age_rule"],"measures_rule":metadata["measures_rule"],"references":references});
    let mut digest = CanonicalDigest::default();
    {
        use std::io::Write;
        let mut writer = std::io::BufWriter::new(&mut digest);
        write_sorted_json(&identity, &mut writer, false)?;
        writer.flush()?;
    }
    let digest = format!("{:x}", digest.0.finalize());
    Ok(format!("{language}-{release}-{}.json", &digest[..12]))
}

/// Include the latest approved configured editions without acquiring new data.
/// Previously published pairs may seed the cache only after edge verification.
#[cfg(feature = "cli")]
pub fn comparison_collections(
    paths: &[std::path::PathBuf],
    output: &std::path::Path,
    allowed: &[String],
) -> DumpResult<(
    Vec<std::path::PathBuf>,
    std::collections::BTreeMap<String, std::path::PathBuf>,
)> {
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};
    let mut supplied = BTreeMap::new();
    for path in paths {
        let metadata = first_metadata(path)?;
        let language = metadata["language"]
            .as_str()
            .ok_or("Lingua grafo mancante")?;
        if !allowed.iter().any(|l| l == language)
            || supplied.insert(language.to_owned(), path.clone()).is_some()
        {
            return Err("Lingua non configurata o raccolta duplicata".into());
        }
    }
    let manifest = match std::fs::read(output.join("manifest.json")) {
        Ok(bytes) => serde_json::from_slice::<Vec<Value>>(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let mut latest = BTreeMap::<String, &Value>::new();
    for entry in &manifest {
        if entry.get("kind").and_then(Value::as_str).unwrap_or("wiki") != "wiki"
            || entry["demo"] == true
        {
            continue;
        }
        let Some(language) = entry["language"]
            .as_str()
            .filter(|l| allowed.iter().any(|a| a == *l))
        else {
            continue;
        };
        let rank = |entry: &Value| {
            (
                entry["date"].as_str().unwrap_or("").to_owned(),
                entry["published_at"].as_str().unwrap_or("").to_owned(),
            )
        };
        if latest
            .get(language)
            .is_none_or(|old| rank(entry) > rank(old))
        {
            latest.insert(language.into(), entry);
        }
    }
    let mut exports = BTreeMap::new();
    for (language, entry) in latest {
        let export = output.join(dataset_filename(&entry["file"])?);
        let metadata = first_metadata(&export)?;
        if metadata["language"] != language
            || metadata["complete"] != true
            || approved_graph(&metadata, false).is_err()
        {
            if supplied.contains_key(&language) {
                continue;
            }
            return Err("Export precedente non verificabile per il confronto incrementale".into());
        }
        exports.insert(language.clone(), export);
        if let std::collections::btree_map::Entry::Vacant(entry) = supplied.entry(language) {
            let language = entry.key();
            let candidates = paths
                .iter()
                .filter_map(|p| p.parent())
                .map(|p| p.join(format!("{language}-raw.json")))
                .filter(|p| p.is_file())
                .collect::<BTreeSet<_>>();
            if candidates.len() != 1 {
                return Err(format!("{language}: conservare il grafo locale già raccolto per estendere il confronto").into());
            }
            let path = candidates
                .into_iter()
                .next()
                .ok_or("Grafo precedente mancante")?;
            if first_metadata(&path)?["language"] != language.as_str() {
                return Err("Lingua del grafo precedente non corrispondente".into());
            }
            entry.insert(path);
        }
    }
    Ok((supplied.into_values().collect(), exports))
}

#[cfg(feature = "cli")]
pub(super) fn available_scientific_name(
    prepared: &PreparedDirectory,
    destination: &std::path::Path,
    name: &str,
) -> DumpResult<String> {
    let source = prepared.path.join(name);
    let existing = destination.join(name);
    if !existing.try_exists()? || file_sha256(&source)? == file_sha256(&existing)? {
        return Ok(name.into());
    }
    // Older filenames did not cover every analysis-only change. Keep their
    // bytes addressable and use the complete result identity for a collision.
    let checksum = file_sha256(&source)?;
    let stem = name.strip_suffix(".json").ok_or("Nome export non valido")?;
    let next = format!("{stem}-{}.json", &checksum[..12]);
    if prepared.path.join(&next).try_exists()? {
        return Err("Identità export duplicata".into());
    }
    std::fs::rename(source, prepared.path.join(&next))?;
    Ok(next)
}

/// Publish all compared members and their aggregate as one prepared generation.
/// A failed move leaves every previous scientific file and manifest usable.
#[cfg(feature = "cli")]
pub fn publish_comparison(
    connection: &rusqlite::Connection,
    output: &std::path::Path,
) -> DumpResult<serde_json::Value> {
    crate::runtime::phase("comparison.publish", || {
        crate::runtime::check_cancelled()?;
        use serde_json::{json, Value};
        use std::collections::{BTreeMap, BTreeSet};
        use std::io::{BufWriter, Write};
        let output = resolved_path(output)?;
        let original = match std::fs::read(output.join("manifest.json")) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let mut manifest: Vec<Value> = original
            .as_deref()
            .map(serde_json::from_slice)
            .transpose()?
            .unwrap_or_default();
        let mut seen = BTreeSet::new();
        for entry in &manifest {
            if !seen.insert(dataset_filename(&entry["file"])?.to_owned()) {
                return Err("Dataset duplicato nel manifest".into());
            }
        }
        let wikis=connection.prepare("SELECT w.language,m.metadata,w.page_count FROM wikis w JOIN comparison_metadata m ON m.language=w.language ORDER BY w.language")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,usize>(2)?)))?.map(|r|{let(l,m,c)=r?;Ok((l,serde_json::from_str::<Value>(&m)?,c))}).collect::<DumpResult<Vec<_>>>()?;
        if wikis.len() < 2
            || wikis.len()
                != connection
                    .query_row("SELECT COUNT(*) FROM wikis", [], |r| r.get::<_, usize>(0))?
        {
            return Err("Confronto incompleto".into());
        }
        let languages = wikis.iter().map(|w| w.0.clone()).collect::<Vec<_>>();
        let (prepared, generation) =
            export_directory(output.parent().ok_or("Directory output non valida")?)?;
        let mut members = Vec::new();
        let mut sources = Vec::new();
        let mut files = BTreeMap::new();
        let mut reused = Vec::new();
        for (language, metadata, count) in &wikis {
            let name = comparison_filename(language, metadata, &languages)?;
            export_comparison_member(connection, language, &prepared.path.join(&name))?;
            let name = available_scientific_name(&prepared, &output, &name)?;
            let member = prepare_member(&prepared, &output.join(&name), &generation)?;
            if member.metadata != *metadata {
                return Err("Metadati del confronto cambiati durante l'export".into());
            }
            let record =
                json!({"language":language,"file":name,"page_count":count,"metadata":metadata});
            let mut source = record.clone();
            source["path"] = json!(if prepared.path.join(&name).is_file() {
                prepared.path.join(&name)
            } else {
                output.join(&name)
            });
            sources.push(source);
            members.push(record);
            files.insert(language.clone(), name);
            manifest.retain(|entry| entry["file"] != member.entry["file"]);
            manifest.push(member.entry);
            reused.extend(member.reused);
        }
        let index = aggregate_index(&members)?;
        let releases = wikis
            .iter()
            .map(|w| w.1["sql_release"].as_str())
            .collect::<BTreeSet<_>>();
        let name = if releases.len() == 1 {
            if let Some(release) = releases
                .iter()
                .next()
                .copied()
                .flatten()
                .filter(|s| s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit()))
            {
                format!(
                    "all-{release}-{}.json",
                    &canonical_json_sha256(&json!(files))?[..12]
                )
            } else {
                format!(
                    "all{}.json",
                    if index["metadata"]["demo"] == true {
                        "-demo"
                    } else {
                        ""
                    }
                )
            }
        } else {
            format!(
                "all{}.json",
                if index["metadata"]["demo"] == true {
                    "-demo"
                } else {
                    ""
                }
            )
        };
        write_atomic(&prepared.path.join(&name), |file| {
            let mut writer = BufWriter::new(file);
            writer.write_all(b"{\"metadata\":")?;
            serde_json::to_writer(&mut writer, &index["metadata"])?;
            writer.write_all(b",\"members\":")?;
            serde_json::to_writer(&mut writer, &index["members"])?;
            writer.write_all(b"}")?;
            writer.flush()?;
            Ok(())
        })?;
        let name = available_scientific_name(&prepared, &output, &name)?;
        let stem = name
            .strip_suffix(".json")
            .ok_or("Nome aggregato non valido")?;
        let summary = format!("{stem}-{generation}-summary.json");
        let creators = format!("{stem}-{generation}-creators.json");
        write_aggregate_summary(
            &sources,
            &prepared.path.join(&summary),
            &index["metadata"],
            true,
        )?;
        reuse_export(&prepared.path.join(&name), &output.join(&name), &mut reused)?;
        let entry = manifest_entry(
            &index["metadata"],
            &name,
            Some(&json!({"summary_file":summary,"creator_index_file":creators})),
        )?;
        manifest.retain(|entry| entry["file"] != name);
        manifest.push(entry);
        DumpClient::<WikimediaHttp>::write_json(
            &prepared.path.join("manifest.json"),
            &json!(manifest),
        )?;
        publish_prepared(
            &prepared.path,
            &output,
            original.as_deref(),
            &reused,
            |from, to| std::fs::rename(from, to),
        )?;
        if wikis.iter().all(|(_, m, _)| {
            m["source"]
                .as_str()
                .is_some_and(|s| s.starts_with("Wikimedia SQL dumps / "))
        }) {
            publish_available_aggregate(&output)?;
        }
        Ok(index)
    })
}

#[cfg(feature = "cli")]
pub(crate) struct PreparedSummaries {
    documents: [serde_json::Value; 2],
    signatures: [(u64, i64); 2],
}

#[cfg(feature = "cli")]
pub(super) fn guard_reused_export(
    path: std::path::PathBuf,
    signature: (u64, i64),
) -> DumpResult<ReusedExport> {
    if file_signature(&path)? != signature {
        return Err("Trasporto riutilizzato modificato durante la verifica".into());
    }
    let checksum = file_sha256(&path)?;
    if file_signature(&path)? != signature {
        return Err("Trasporto riutilizzato modificato durante la verifica".into());
    }
    Ok(ReusedExport {
        path,
        signature,
        checksum,
    })
}

#[cfg(feature = "cli")]
pub(crate) fn prepared_summaries(
    summary_path: &std::path::Path,
    creators_path: &std::path::Path,
    metadata: &serde_json::Value,
    sources: &serde_json::Value,
    count: usize,
    allow_coverage_change: bool,
) -> Option<PreparedSummaries> {
    use serde_json::{json, Value};
    use std::io::BufReader;
    let read = |path: &std::path::Path| -> DumpResult<(Value, (u64, i64))> {
        let signature = file_signature(path)?;
        let value = serde_json::from_reader(BufReader::new(std::fs::File::open(path)?))?;
        if file_signature(path)? != signature {
            return Err("Riepilogo modificato durante la verifica".into());
        }
        Ok((value, signature))
    };
    let (summary, summary_signature) = read(summary_path).ok()?;
    let (creators, creators_signature) = read(creators_path).ok()?;
    let reduced = |value: &Value| -> Option<Value> {
        let mut fields = value.as_object()?.clone();
        if metadata["kind"] == "aggregate" {
            fields.remove("creators");
            fields.remove("history");
        }
        if allow_coverage_change {
            fields.remove("comparison_coverage");
        }
        Some(fields.into())
    };
    let preview = summary["article_preview"].as_array()?;
    let orphans = summary["reports"]["overview"]["orphans"].as_u64()?;
    if summary["format"] != "dashboard_summary_v1"
        || summary["histogram_rule"] != "shared_p2_p98_tails_v1"
        || summary["reports"]["numeric"]["creator_continuity_months_before_creation"]
            ["month_histogram"]["rule"]
            != "months_0_12_overflow_v1"
        || summary["measures_rule"]
            != metadata
                .get("measures_rule")
                .cloned()
                .unwrap_or(json!("creation_quality_creators_v3"))
        || !summary["reports"].is_object()
        || summary["reports"]["overview"]["total"].as_u64() != Some(count as u64)
        || preview.len() != usize::try_from(orphans.min(25)).ok()?
        || preview.iter().any(|p| {
            p["orphan"] != true || p["page_id"].as_i64().is_none() || !p["title"].is_string()
        })
        || creators["format"] != "creator_index_v1"
        || !creators["creators"].is_array()
        || reduced(&summary["metadata"])? != reduced(metadata)?
        || creators["metadata"] != summary["metadata"]
        || summary["sources"] != *sources
        || creators["sources"] != *sources
        || summary["census_page_count"].as_u64() != Some(count as u64)
        || creators["census_page_count"].as_u64() != Some(count as u64)
    {
        return None;
    }
    Some(PreparedSummaries {
        documents: [summary, creators],
        signatures: [summary_signature, creators_signature],
    })
}

/// Publish the latest complete real editions, labelling partial cross-language
/// coverage. Reuse only current summaries with exact canonical fingerprints.
/// All repaired or adapted sidecars use new names and manifest approval is last.
#[cfg(feature = "cli")]
pub fn publish_available_aggregate(
    output: &std::path::Path,
) -> DumpResult<Option<serde_json::Value>> {
    crate::runtime::phase("dashboard.available", || {
        crate::runtime::check_cancelled()?;
        use serde_json::{json, Value};
        use std::collections::{BTreeMap, BTreeSet};
        use std::io::{BufWriter, Read, Write};
        let original = std::fs::read(output.join("manifest.json"))?;
        let mut manifest: Vec<Value> = serde_json::from_slice(&original)?;
        let mut latest = BTreeMap::<String, usize>::new();
        for (position, entry) in manifest.iter().enumerate() {
            if entry["kind"] == "aggregate" || entry["demo"] == true {
                continue;
            }
            let language = entry["language"]
                .as_str()
                .ok_or("Lingua manifest mancante")?;
            crate::wiki_database(language, None)?;
            dataset_filename(&entry["file"])?;
            dataset_filename(&entry["cards_file"])?;
            let rank = |entry: &Value| {
                (
                    entry["date"].as_str().unwrap_or("").to_owned(),
                    entry["published_at"].as_str().unwrap_or("").to_owned(),
                )
            };
            if latest
                .get(language)
                .is_none_or(|old| rank(entry) > rank(&manifest[*old]))
            {
                latest.insert(language.into(), position);
            }
        }
        if latest.len() < 2 {
            return Ok(None);
        }
        let mut members = Vec::new();
        let mut sources = Vec::new();
        let mut signatures = Vec::new();
        let mut files = BTreeMap::new();
        let count_pattern = regex::bytes::Regex::new(r#""census_page_count"\s*:\s*(\d+)"#)?;
        for (language, position) in &latest {
            let entry = &manifest[*position];
            let name = dataset_filename(&entry["file"])?;
            let path = output.join(name);
            let metadata = first_metadata(&path)?;
            if metadata["language"] != *language
                || metadata["demo"] == true
                || metadata["complete"] != true
                || approved_graph(&metadata, false).is_err()
            {
                return Err(
                    "Export precedente non verificabile per l'aggregato disponibile".into(),
                );
            }
            let mut header = Vec::new();
            std::fs::File::open(output.join(dataset_filename(&entry["cards_file"])?))?
                .take(1024 * 1024)
                .read_to_end(&mut header)?;
            let count = count_pattern
                .captures(&header)
                .and_then(|m| m.get(1))
                .ok_or("Trasporto delle schede incompleto")?
                .as_bytes();
            let count = std::str::from_utf8(count)?.parse::<usize>()?;
            let signature = file_signature(&path)?;
            signatures.push((path.clone(), signature));
            files.insert(language.clone(), name.to_owned());
            members.push(json!({"language":language,"file":name,"path":path,"page_count":count,"metadata":metadata}));
            sources.push(
                json!({"language":language,"file":name,"size":signature.0,"mtime_ns":signature.1}),
            );
        }
        let mut index = aggregate_index(&members)?;
        let expected = latest.keys().cloned().collect::<BTreeSet<_>>();
        let complete = members.iter().all(|m| {
            let language = m["language"].as_str().unwrap_or("");
            let references = m["metadata"]["comparison"]["reference_languages"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            references
                == expected
                    .iter()
                    .filter(|l| l.as_str() != language)
                    .cloned()
                    .collect()
        });
        index["metadata"]["comparison_coverage"] =
            json!(if complete { "complete" } else { "partial" });
        let count = members
            .iter()
            .try_fold(0usize, |sum, m| {
                sum.checked_add(usize::try_from(m["page_count"].as_u64()?).ok()?)
            })
            .ok_or("Denominatore aggregato fuori intervallo")?;
        let fingerprints = json!(sources);
        let (prepared, generation) =
            export_directory(output.parent().ok_or("Directory output non valida")?)?;
        let mut reused = Vec::new();
        for (language, position) in &latest {
            let member = members
                .iter()
                .find(|m| m["language"] == *language)
                .ok_or("Membro aggregato mancante")?;
            let entry = &mut manifest[*position];
            let fingerprint = json!([fingerprints
                .as_array()
                .ok_or("Fingerprint aggregati non validi")?
                .iter()
                .find(|s| s["language"] == *language)
                .ok_or("Fingerprint membro mancante")?]);
            let pair = entry["summary_file"]
                .as_str()
                .zip(entry["creator_index_file"].as_str())
                .and_then(|(s, c)| {
                    dataset_filename(&json!(s)).ok()?;
                    dataset_filename(&json!(c)).ok()?;
                    prepared_summaries(
                        &output.join(s),
                        &output.join(c),
                        &member["metadata"],
                        &fingerprint,
                        member["page_count"].as_u64()? as usize,
                        false,
                    )
                });
            if let Some(pair) = pair {
                for (key, signature) in ["summary_file", "creator_index_file"]
                    .into_iter()
                    .zip(pair.signatures)
                {
                    reused.push(guard_reused_export(
                        output.join(dataset_filename(&entry[key])?),
                        signature,
                    )?);
                }
                continue;
            }
            let stem = dataset_filename(&entry["file"])?
                .strip_suffix(".json")
                .ok_or("Nome membro non valido")?;
            let name = format!("{stem}-{generation}-summary.json");
            write_aggregate_summary(
                std::slice::from_ref(member),
                &prepared.path.join(&name),
                &member["metadata"],
                false,
            )?;
            let creators_name = format!("{stem}-{generation}-creators.json");
            entry["summary_file"] = json!(name);
            entry["creator_index_file"] = json!(creators_name);
        }
        let release = index["metadata"]["finished_at"]
            .as_str()
            .ok_or("Fine aggregato mancante")?
            .chars()
            .take(10)
            .filter(|c| *c != '-')
            .collect::<String>();
        let name = format!(
            "all-{release}-available-{}.json",
            &canonical_json_sha256(&json!(files))?[..12]
        );
        write_atomic(&prepared.path.join(&name), |file| {
            let mut writer = BufWriter::new(file);
            writer.write_all(b"{\"metadata\":")?;
            serde_json::to_writer(&mut writer, &index["metadata"])?;
            writer.write_all(b",\"members\":")?;
            serde_json::to_writer(&mut writer, &index["members"])?;
            writer.write_all(b"}")?;
            writer.flush()?;
            Ok(())
        })?;
        let name = available_scientific_name(&prepared, output, &name)?;
        let mut matched = None;
        for allow_change in [false, true] {
            for entry in manifest.iter().rev().filter(|e| e["kind"] == "aggregate") {
                let Some((summary, creators)) = entry["summary_file"]
                    .as_str()
                    .zip(entry["creator_index_file"].as_str())
                else {
                    continue;
                };
                if dataset_filename(&json!(summary)).is_err()
                    || dataset_filename(&json!(creators)).is_err()
                {
                    continue;
                }
                if let Some(pair) = prepared_summaries(
                    &output.join(summary),
                    &output.join(creators),
                    &index["metadata"],
                    &fingerprints,
                    count,
                    allow_change,
                ) {
                    matched = Some((summary.to_owned(), creators.to_owned(), pair, allow_change));
                    break;
                }
            }
            if matched.is_some() {
                break;
            }
        }
        let stem = name
            .strip_suffix(".json")
            .ok_or("Nome aggregato disponibile non valido")?;
        let new_summary = format!("{stem}-{generation}-summary.json");
        let new_creators = format!("{stem}-{generation}-creators.json");
        let (summary, creators) = if let Some((summary, creators, mut pair, allow_change)) = matched
        {
            if !allow_change {
                for (file, signature) in [&summary, &creators].into_iter().zip(pair.signatures) {
                    reused.push(guard_reused_export(output.join(file), signature)?);
                }
                (summary, creators)
            } else {
                let mut meta = pair.documents[0]["metadata"].clone();
                meta.as_object_mut()
                    .ok_or("Metadati del riepilogo non validi")?
                    .extend(
                        index["metadata"]
                            .as_object()
                            .ok_or("Metadati aggregato non validi")?
                            .clone(),
                    );
                pair.documents[0]["metadata"] = meta.clone();
                pair.documents[1]["metadata"] = meta;
                DumpClient::<WikimediaHttp>::write_json(
                    &prepared.path.join(&new_summary),
                    &pair.documents[0],
                )?;
                DumpClient::<WikimediaHttp>::write_json(
                    &prepared.path.join(&new_creators),
                    &pair.documents[1],
                )?;
                (new_summary, new_creators)
            }
        } else {
            write_aggregate_summary(
                &members,
                &prepared.path.join(&new_summary),
                &index["metadata"],
                true,
            )?;
            (new_summary, new_creators)
        };
        reuse_export(&prepared.path.join(&name), &output.join(&name), &mut reused)?;
        let entry = manifest_entry(
            &index["metadata"],
            &name,
            Some(&json!({"summary_file":summary,"creator_index_file":creators})),
        )?;
        manifest.retain(|e| e["file"] != name);
        manifest.push(entry);
        for (path, signature) in signatures {
            if file_signature(&path)? != signature {
                return Err("Membro modificato durante l'aggregazione".into());
            }
        }
        DumpClient::<WikimediaHttp>::write_json(
            &prepared.path.join("manifest.json"),
            &json!(manifest),
        )?;
        publish_prepared(
            &prepared.path,
            output,
            Some(&original),
            &reused,
            |from, to| std::fs::rename(from, to),
        )?;
        Ok(Some(index))
    })
}

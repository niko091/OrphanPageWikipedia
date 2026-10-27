use super::DumpResult;
#[cfg(feature = "cli")]
use super::{
    canonical_json_sha256, file_signature, resolved_path, stage_export, write_atomic,
    CanonicalExport, PreparedDirectory, ARTICLE_INDEX_COLUMNS, OBSERVATION_COLUMNS, TRANSPORT_RULE,
};

#[cfg(feature = "cli")]
pub(super) fn write_browser_projection(
    path: &std::path::Path,
    staged: &CanonicalExport,
    format: &str,
    columns: &[&str],
    include_count: bool,
    card_identity: Option<&str>,
) -> DumpResult<()> {
    use serde_json::{json, Value};
    use std::io::{BufWriter, Write};
    write_atomic(path, |file| {
        let mut writer = BufWriter::with_capacity(65536, file);
        writer.write_all(b"{\"format\":")?;
        serde_json::to_writer(&mut writer, format)?;
        writer.write_all(b",\"metadata\":")?;
        serde_json::to_writer(&mut writer, &staged.metadata)?;
        if include_count {
            writer.write_all(format!(",\"census_page_count\":{}", staged.count).as_bytes())?;
        }
        if let Some(identity) = card_identity {
            writer.write_all(b",\"card_identity\":")?;
            serde_json::to_writer(&mut writer, identity)?;
        }
        writer.write_all(b",\"columns\":")?;
        serde_json::to_writer(&mut writer, columns)?;
        writer.write_all(b",\"rows\":[")?;
        let mut pages = staged.database.prepare(if card_identity.is_some() {
            "SELECT data FROM pages WHERE orphan=1 ORDER BY position"
        } else {
            "SELECT data FROM pages ORDER BY position"
        })?;
        let mut rows = pages.query([])?;
        let mut position = 0;
        while let Some(row) = rows.next()? {
            let page: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
            if position != 0 {
                writer.write_all(b",")?;
            }
            let projected = columns.iter().map(|key| &page[key]).collect::<Vec<_>>();
            serde_json::to_writer(&mut writer, &projected)?;
            position += 1;
        }
        let expected = if card_identity.is_some() {
            staged
                .database
                .query_row("SELECT COUNT(*) FROM pages WHERE orphan=1", [], |row| {
                    row.get::<_, usize>(0)
                })?
        } else {
            staged.count
        };
        if position != expected {
            return Err("Proiezione del censimento incompleta".into());
        }
        writer.write_all(b"],\"link_candidates\":{")?;
        let mut query = staged.database.prepare(if card_identity.is_some() {
            "SELECT s.id,s.data FROM shortlists s JOIN candidates c ON c.id=s.id ORDER BY c.position"
        } else { "SELECT id,status,count FROM candidates ORDER BY position" })?;
        let mut rows = query.query([])?;
        let mut first = true;
        while let Some(row) = rows.next()? {
            if !first {
                writer.write_all(b",")?;
            }
            first = false;
            serde_json::to_writer(&mut writer, &row.get::<_, i64>(0)?.to_string())?;
            writer.write_all(b":")?;
            if card_identity.is_some() {
                // This JSON was serialized and indexed in this private database.
                writer.write_all(row.get::<_, String>(1)?.as_bytes())?;
            } else {
                serde_json::to_writer(
                    &mut writer,
                    &json!({
                        "status":row.get::<_, Option<String>>(1)?,
                        "candidate_count":row.get::<_, Option<i64>>(2)?
                    }),
                )?;
            }
        }
        writer.write_all(b"}}")?;
        writer.flush()?;
        Ok(())
    })
}

#[cfg(feature = "cli")]
pub(super) fn write_profile_topics(
    directory: &std::path::Path,
    staged: &CanonicalExport,
) -> DumpResult<()> {
    crate::runtime::phase("dashboard.profile_topics", || {
        crate::runtime::check_cancelled()?;
        use serde_json::{json, Value};
        use std::io::{BufWriter, Write};
        std::fs::create_dir(directory)?;
        let mut query = staged
            .database
            .prepare("SELECT id,data FROM pages ORDER BY id")?;
        let mut records = query.query([])?;
        let mut bucket = None;
        let mut rows = Vec::new();
        let write_shard = |bucket: Option<i64>, rows: &[Value]| -> DumpResult<()> {
            if let Some(bucket) = bucket {
                #[derive(serde::Serialize)]
                struct Shard<'a> {
                    format: &'static str,
                    metadata: &'a Value,
                    bucket: i64,
                    rows: &'a [Value],
                }
                write_atomic(&directory.join(format!("{bucket}.json")), |file| {
                    let mut writer = BufWriter::with_capacity(65536, file);
                    serde_json::to_writer(
                        &mut writer,
                        &Shard {
                            format: "profile_topics_shard_v1",
                            metadata: &staged.metadata,
                            bucket,
                            rows,
                        },
                    )?;
                    writer.flush()?;
                    Ok(())
                })?;
            }
            Ok(())
        };
        while let Some(record) = records.next()? {
            let id = record.get::<_, i64>(0)?;
            let next = Some(id / 1024);
            if next != bucket {
                write_shard(bucket, &rows)?;
                rows.clear();
                bucket = next;
            }
            let page: Value = serde_json::from_str(&record.get::<_, String>(1)?)?;
            rows.push(json!([id, page["topic_status"], page["topic_labels"]]));
        }
        write_shard(bucket, &rows)?;
        write_atomic(&directory.join("index.json"), |file| {
            let mut writer = BufWriter::with_capacity(65536, file);
            writer.write_all(b"{\"format\":\"profile_topics_index_v1\",\"metadata\":")?;
            serde_json::to_writer(&mut writer, &staged.metadata)?;
            writer.write_all(
                format!(
                    ",\"census_page_count\":{},\"shard_size\":1024,\"shards\":[",
                    staged.count
                )
                .as_bytes(),
            )?;
            let mut query = staged
                .database
                .prepare("SELECT DISTINCT id/1024 AS bucket FROM pages ORDER BY bucket")?;
            let mut rows = query.query([])?;
            let mut first = true;
            while let Some(row) = rows.next()? {
                if !first {
                    writer.write_all(b",")?;
                }
                first = false;
                serde_json::to_writer(&mut writer, &row.get::<_, i64>(0)?)?;
            }
            writer.write_all(b"]}")?;
            writer.flush()?;
            Ok(())
        })
    })
}

#[cfg(feature = "cli")]
#[derive(serde::Serialize)]
pub(crate) struct EditorialCandidate<'a> {
    #[serde(flatten)]
    pub(super) fields: std::collections::BTreeMap<&'a str, &'a serde_json::Value>,
    evidence: [&'a serde_json::Value; 1],
}

#[cfg(feature = "cli")]
pub(crate) fn editorial_shortlist<'a>(
    record: &'a serde_json::Value,
    target: i64,
    language: &str,
) -> DumpResult<(Vec<EditorialCandidate<'a>>, Vec<i64>)> {
    use serde_json::Value;
    // Python str.strip also treats these four C0 separators as whitespace.
    let nonempty = |value: &Value| {
        value.as_str().is_some_and(|text| {
            !text
                .trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
                .is_empty()
        })
    };
    let mut selected = Vec::with_capacity(10);
    let mut ids = Vec::with_capacity(10);
    let mut sizes = [0; 2];
    let candidates = match record.get("candidates") {
        None => return Ok((selected, ids)),
        Some(Value::Array(candidates)) => candidates,
        _ => return Err("Lista delle candidate non valida".into()),
    };
    for candidate in candidates {
        let fields = candidate.as_object().ok_or("Candidata non valida")?;
        let group = usize::from(candidate["dead_end"] != true);
        let Some(id) = candidate["page_id"].as_i64().filter(|id| *id > 0) else {
            continue;
        };
        if sizes[group] >= 5 || id == target || ids.contains(&id) || !nonempty(&candidate["title"])
        {
            continue;
        }
        let evidence = match candidate.get("evidence") {
            None => continue,
            Some(Value::Array(evidence)) => evidence,
            _ => return Err("Lista delle evidenze non valida".into()),
        };
        for edge in evidence {
            if !edge.is_object() {
                return Err("Evidenza non valida".into());
            }
            if !edge["language"]
                .as_str()
                .is_some_and(|code| code != language && crate::wiki_database(code, None).is_ok())
                || !nonempty(&edge["source_title"])
                || !nonempty(&edge["target_title"])
            {
                continue;
            }
            selected.push(EditorialCandidate {
                fields: fields
                    .iter()
                    .filter(|(key, _)| key.as_str() != "evidence")
                    .map(|(key, value)| (key.as_str(), value))
                    .collect(),
                evidence: [edge],
            });
            ids.push(id);
            sizes[group] += 1;
            break;
        }
    }
    Ok((selected, ids))
}

#[cfg(feature = "cli")]
pub(super) fn write_editorial_exports(
    folder: &std::path::Path,
    names: &serde_json::Value,
    staged: &CanonicalExport,
    card_identity: &str,
    connection_identity: &str,
) -> DumpResult<()> {
    crate::runtime::phase("dashboard.editorial", || {
        crate::runtime::check_cancelled()?;
        use serde_json::{json, Value};
        use std::io::{BufWriter, Write};
        let path = |key: &str| -> DumpResult<std::path::PathBuf> {
            Ok(folder.join(
                names[key]
                    .as_str()
                    .ok_or("Nome trasporto editoriale mancante")?,
            ))
        };
        let cards = path("cards_dir")?;
        let connections = path("connections_dir")?;
        std::fs::create_dir(&cards)?;
        std::fs::create_dir(&connections)?;
        // The connection is private and stage_export has committed its transaction.
        // Group every shortlist INSERT rather than syncing once per orphan.
        let transaction = staged.database.unchecked_transaction()?;
        transaction
            .execute_batch("CREATE TABLE shortlists(id INTEGER PRIMARY KEY,data TEXT NOT NULL)")?;
        let mut insert = transaction.prepare("INSERT INTO shortlists VALUES(?,?)")?;
        let mut query = transaction.prepare("SELECT p.id,p.data,c.data FROM pages p LEFT JOIN candidates c ON c.id=p.id WHERE p.orphan=1 ORDER BY p.position")?;
        let mut rows = query.query([])?;
        let language = staged.metadata["language"]
            .as_str()
            .ok_or("Lingua mancante")?;
        while let Some(row) = rows.next()? {
            let id = row.get::<_, i64>(0)?;
            let page: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
            let encoded = row.get::<_, Option<String>>(2)?;
            let record: Value = encoded
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or(json!({}));
            let (selected, candidate_ids) = editorial_shortlist(&record, id, language)?;
            if encoded.is_some() {
                let summary = json!({"status":record["status"],"candidate_count":record["candidate_count"],"candidate_ids":candidate_ids});
                insert.execute(rusqlite::params![id, serde_json::to_string(&summary)?])?;
            }
            #[derive(serde::Serialize)]
            struct Detail<'a> {
                format: &'static str,
                connection_identity: &'a str,
                census_page_count: usize,
                pages: [&'a Value; 1],
                link_candidates: std::collections::BTreeMap<String, &'a Value>,
            }
            write_atomic(&connections.join(format!("{id}.json")), |file| {
                let mut writer = BufWriter::with_capacity(65536, file);
                serde_json::to_writer(
                    &mut writer,
                    &Detail {
                        format: "connections_v2",
                        connection_identity,
                        census_page_count: staged.count,
                        pages: [&page],
                        link_candidates: [(id.to_string(), &record)].into(),
                    },
                )?;
                writer.flush()?;
                Ok(())
            })?;
            #[derive(serde::Serialize)]
            struct CandidateRecord<'a> {
                status: &'a Value,
                candidate_count: &'a Value,
                candidates: &'a [EditorialCandidate<'a>],
            }
            #[derive(serde::Serialize)]
            struct Card<'a> {
                format: &'static str,
                card_identity: &'a str,
                census_page_count: usize,
                page: &'a Value,
                candidate_record: Option<CandidateRecord<'a>>,
            }
            write_atomic(&cards.join(format!("{id}.json")), |file| {
                let mut writer = BufWriter::with_capacity(65536, file);
                serde_json::to_writer(
                    &mut writer,
                    &Card {
                        format: "orphan_card_v1",
                        card_identity,
                        census_page_count: staged.count,
                        page: &page,
                        candidate_record: encoded.is_some().then_some(CandidateRecord {
                            status: &record["status"],
                            candidate_count: &record["candidate_count"],
                            candidates: &selected,
                        }),
                    },
                )?;
                writer.flush()?;
                Ok(())
            })?;
        }
        drop(rows);
        drop(query);
        drop(insert);
        transaction.commit()?;
        #[derive(serde::Serialize)]
        struct Metadata<'a> {
            format: &'static str,
            connection_identity: &'a str,
            metadata: &'a Value,
            census_page_count: usize,
        }
        write_atomic(&path("connections_metadata_file")?, |file| {
            let mut writer = BufWriter::with_capacity(65536, file);
            serde_json::to_writer(
                &mut writer,
                &Metadata {
                    format: "connections_metadata_v1",
                    connection_identity,
                    metadata: &staged.metadata,
                    census_page_count: staged.count,
                },
            )?;
            writer.flush()?;
            Ok(())
        })?;
        write_browser_projection(
            &path("cards_file")?,
            staged,
            "orphan_cards_v2",
            ARTICLE_INDEX_COLUMNS,
            true,
            Some(card_identity),
        )
    })
}

#[cfg(feature = "cli")]
pub(super) struct SummaryPreview {
    title: String,
    language: String,
    id: i64,
    pub(super) fields: serde_json::Value,
}

#[cfg(feature = "cli")]
pub(super) fn add_article_preview(
    preview: &mut Vec<SummaryPreview>,
    page: &serde_json::Value,
    language: Option<&str>,
) -> DumpResult<()> {
    if page["orphan"] != true {
        return Ok(());
    }
    let title = page["title"].as_str().ok_or("Titolo mancante")?;
    let id = page["page_id"].as_i64().ok_or("ID mancante")?;
    let code = language.unwrap_or("");
    let position = preview.partition_point(|old| {
        focaccia::unicode_full_casecmp(&old.title, title)
            .then_with(|| old.title.as_str().cmp(title))
            .then_with(|| old.language.as_str().cmp(code))
            .then_with(|| old.id.cmp(&id))
            .is_lt()
    });
    if position < 25 {
        let mut fields: serde_json::Map<String, serde_json::Value> = OBSERVATION_COLUMNS
            .iter()
            .map(|key| (String::from(*key), page[*key].clone()))
            .collect();
        if let Some(language) = language {
            fields.insert("language".into(), serde_json::json!(language));
        }
        preview.insert(
            position,
            SummaryPreview {
                title: title.into(),
                language: code.into(),
                id,
                fields: fields.into(),
            },
        );
        preview.truncate(25);
    }
    Ok(())
}

#[cfg(feature = "cli")]
pub(super) fn write_dashboard_summaries(
    source: &std::path::Path,
    folder: &std::path::Path,
    names: &serde_json::Value,
    staged: &CanonicalExport,
) -> DumpResult<()> {
    crate::runtime::phase("dashboard.summary", || {
        crate::runtime::check_cancelled()?;
        use serde_json::{json, Value};
        let metadata = &staged.metadata;
        let collected =
            |status: &Value| matches!(status.as_str(), Some("collected" | "partially_collected"));
        let mut summary = crate::dashboard::SummaryAccumulator::new(
            metadata["language"]
                .as_str()
                .ok_or("Lingua mancante")?
                .into(),
            false,
            !collected(&metadata["creators"]["status"])
                && collected(&metadata["history"]["status"]),
        );
        let mut preview = Vec::<SummaryPreview>::with_capacity(26);
        let mut query = staged
            .database
            .prepare("SELECT data FROM pages ORDER BY position")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let page: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
            summary.add_observation(&page, None)?;
            add_article_preview(&mut preview, &page, None)?;
        }
        if summary.len() != staged.count {
            return Err("Riepilogo del censimento incompleto".into());
        }
        let sources = json!([{"language":metadata["language"],"file":source.file_name().and_then(|s|s.to_str()).ok_or("Nome raccolta non valido")?,
        "size":staged.signature.0,"mtime_ns":staged.signature.1}]);
        let article_preview = preview
            .into_iter()
            .map(|entry| entry.fields)
            .collect::<Vec<_>>();
        write_summary_files(
            folder,
            names,
            metadata,
            staged.count,
            summary,
            article_preview,
            sources,
        )
    })
}

#[cfg(feature = "cli")]
pub(super) fn write_summary_files(
    folder: &std::path::Path,
    names: &serde_json::Value,
    metadata: &serde_json::Value,
    count: usize,
    summary: crate::dashboard::SummaryAccumulator,
    article_preview: Vec<serde_json::Value>,
    sources: serde_json::Value,
) -> DumpResult<()> {
    use serde_json::{json, Value};
    use std::io::{BufWriter, Write};
    let default_rule = json!("creation_quality_creators_v3");
    let encoded = summary.finish()?;
    drop(summary);
    let mut reports: Value = serde_json::from_str(&encoded)?;
    drop(encoded);
    let creators = reports
        .as_object_mut()
        .ok_or("Riepilogo non valido")?
        .remove("creators")
        .ok_or("Indice autori mancante")?;
    reports["categories"]
        .as_array_mut()
        .ok_or("Riepilogo categorie mancante")?
        .truncate(20);
    #[derive(serde::Serialize)]
    struct Summary<'a> {
        format: &'static str,
        histogram_rule: &'static str,
        measures_rule: &'a Value,
        metadata: &'a Value,
        sources: &'a Value,
        census_page_count: usize,
        reports: &'a Value,
        article_preview: &'a [Value],
    }
    write_atomic(
        &folder.join(
            names["summary_file"]
                .as_str()
                .ok_or("Nome riepilogo mancante")?,
        ),
        |file| {
            let mut writer = BufWriter::with_capacity(65536, file);
            serde_json::to_writer(
                &mut writer,
                &Summary {
                    format: "dashboard_summary_v1",
                    histogram_rule: "shared_p2_p98_tails_v1",
                    measures_rule: metadata.get("measures_rule").unwrap_or(&default_rule),
                    metadata,
                    sources: &sources,
                    census_page_count: count,
                    reports: &reports,
                    article_preview: &article_preview,
                },
            )?;
            writer.flush()?;
            Ok(())
        },
    )?;
    #[derive(serde::Serialize)]
    struct Creators<'a> {
        format: &'static str,
        metadata: &'a Value,
        sources: &'a Value,
        census_page_count: usize,
        creators: &'a Value,
    }
    write_atomic(
        &folder.join(
            names["creator_index_file"]
                .as_str()
                .ok_or("Nome indice autori mancante")?,
        ),
        |file| {
            let mut writer = BufWriter::with_capacity(65536, file);
            serde_json::to_writer(
                &mut writer,
                &Creators {
                    format: "creator_index_v1",
                    metadata,
                    sources: &sources,
                    census_page_count: count,
                    creators: &creators,
                },
            )?;
            writer.flush()?;
            Ok(())
        },
    )
}

/// Prepare all member dashboard transports in a NEW private directory. This
/// does not publish an approval manifest. Only the owned directory is removed
/// on error. The existing chart accumulator retains compact per-page measures;
/// graph/candidate vectors are never retained across target records.
/// Source stat fingerprints and generation names follow the Python transport
/// contract; previous generations and the canonical source are never rewritten.
#[cfg(feature = "cli")]
pub fn prepare_dashboard_exports(
    source: &std::path::Path,
    folder: &std::path::Path,
    expected: Option<&serde_json::Value>,
    transport_generation: Option<&str>,
) -> DumpResult<serde_json::Value> {
    crate::runtime::phase("dashboard.member", || {
        crate::runtime::check_cancelled()?;
        use serde_json::json;
        if folder.try_exists()? || resolved_path(source)?.starts_with(resolved_path(folder)?) {
            return Err("Serve una nuova directory privata distinta dal dataset".into());
        }
        if transport_generation.is_some_and(|generation| {
            generation.len() != 32
                || !generation
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err("Generazione dei trasporti non valida".into());
        }
        let staged = stage_export(source)?;
        let metadata = &staged.metadata;
        let demo = |value: &serde_json::Value| -> DumpResult<bool> {
            match value.get("demo") {
                None | Some(serde_json::Value::Null) => Ok(false),
                Some(serde_json::Value::Bool(value)) => Ok(*value),
                _ => Err("Indicatore demo non valido".into()),
            }
        };
        if let Some(expected) = expected {
            if metadata["language"] != expected["language"]
                || metadata["finished_at"] != expected["date"]
                || demo(metadata)? != demo(expected)?
            {
                return Err("Manifest e raccolta non corrispondenti".into());
            }
        }
        let fingerprint = canonical_json_sha256(&json!([
            TRANSPORT_RULE,
            metadata,
            staged.signature.0,
            staged.signature.1
        ]))?;
        let generation = transport_generation.unwrap_or(&fingerprint[..12]);
        let stem = format!(
            "{}-{generation}",
            source
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or("Nome del dataset non valido")?
        );
        let names = json!({
            "observations_file":format!("{stem}-observations.json"),
            "articles_file":format!("{stem}-articles.json"),
            "profile_topics_dir":format!("{stem}-profile-topics"),
            "cards_file":format!("{stem}-cards-v2.json"),
            "cards_dir":format!("{stem}-cards"),
            "connections_dir":format!("{stem}-connections"),
            "connections_metadata_file":format!("{stem}-connections-metadata.json"),
            "summary_file":format!("{stem}-summary.json"),
            "creator_index_file":format!("{stem}-creators.json")
        });
        let mut prepared = PreparedDirectory::new(folder)?;
        for (key, format, columns, count) in [
            (
                "observations_file",
                "observations_v1",
                OBSERVATION_COLUMNS,
                false,
            ),
            (
                "articles_file",
                "article_index_v1",
                ARTICLE_INDEX_COLUMNS,
                true,
            ),
        ] {
            write_browser_projection(
                &folder.join(names[key].as_str().ok_or("Nome indice mancante")?),
                &staged,
                format,
                columns,
                count,
                None,
            )?;
        }
        write_profile_topics(
            &folder.join(
                names["profile_topics_dir"]
                    .as_str()
                    .ok_or("Directory temi mancante")?,
            ),
            &staged,
        )?;
        let identity = |format: &str| {
            canonical_json_sha256(&json!([
                format,
                metadata,
                staged.signature.0,
                staged.signature.1,
                transport_generation
            ]))
        };
        write_editorial_exports(
            folder,
            &names,
            &staged,
            &identity("orphan_cards_v2")?,
            &identity("connections_v2")?,
        )?;
        write_dashboard_summaries(source, folder, &names, &staged)?;
        if file_signature(source)? != staged.signature {
            return Err("Esportazione modificata durante la conversione".into());
        }
        prepared.complete = true;
        Ok(names)
    })
}

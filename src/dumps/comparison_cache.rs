use super::DumpResult;
#[cfg(feature = "cli")]
use super::{
    approved_graph, canonical_json_sha256, comparison_schema, export_directory, file_sha256,
    file_signature, first_metadata, match_comparison, resolved_path, stage_comparison_graph,
    stage_export, AnalysisOptions, DumpClient, PublicationLock, WikimediaHttp,
};

#[cfg(feature = "cli")]
pub(super) const COMPARISON_CACHE_RULE: &str = "verified_graph_and_ordered_pairs_v1";

#[cfg(feature = "cli")]
#[derive(Clone)]
pub(super) struct ComparisonInput {
    graph: String,
    analysis: String,
    sources: std::collections::BTreeMap<String, Option<std::path::PathBuf>>,
}

/// Complete source lists and the operational work root for an incremental
/// comparison. The configured pool controls retained previous editions too.
#[cfg(feature = "cli")]
pub struct ComparisonOptions<'a> {
    pub rules: &'a serde_json::Value,
    pub work_dir: &'a std::path::Path,
    pub topics: &'a [std::path::PathBuf],
    pub histories: &'a [std::path::PathBuf],
    pub qualities: &'a [std::path::PathBuf],
    pub allowed_languages: Option<&'a [String]>,
    pub published: Option<&'a std::collections::BTreeMap<String, std::path::PathBuf>>,
}

#[cfg(feature = "cli")]
pub(super) fn comparison_signature(path: &std::path::Path) -> DumpResult<(u64, i64, i64, u64)> {
    let (size, mtime) = file_signature(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let stat = std::fs::metadata(path)?;
        Ok((
            size,
            mtime,
            stat.ctime()
                .checked_mul(1_000_000_000)
                .and_then(|s| s.checked_add(stat.ctime_nsec()))
                .ok_or("Data del file fuori intervallo")?,
            stat.ino(),
        ))
    }
    #[cfg(not(unix))]
    {
        Ok((size, mtime, mtime, 0))
    }
}

#[cfg(feature = "cli")]
pub(super) fn comparison_source(
    raw: &std::path::Path,
    rules: &serde_json::Value,
    supplied: &std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, std::path::PathBuf>,
    >,
    signatures: &mut std::collections::BTreeMap<std::path::PathBuf, (u64, i64, i64, u64)>,
) -> DumpResult<(String, ComparisonInput)> {
    use serde_json::json;
    let raw = resolved_path(raw)?;
    let metadata = first_metadata(&raw)?;
    approved_graph(&metadata, true)?;
    let language = metadata["language"]
        .as_str()
        .ok_or("Lingua mancante")?
        .to_owned();
    let mut sources = std::collections::BTreeMap::from([("raw".to_owned(), Some(raw.clone()))]);
    let stem = raw
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Nome input non valido")?;
    for kind in ["topics", "history", "quality"] {
        let explicit = supplied.get(kind).and_then(|s| s.get(&language)).cloned();
        let sibling = raw.with_file_name(format!(
            "{}-{kind}.json",
            stem.strip_suffix("-raw").unwrap_or(stem)
        ));
        let path = explicit.or_else(|| sibling.is_file().then_some(sibling));
        sources.insert(kind.into(), path.as_deref().map(resolved_path).transpose()?);
    }
    let mut digests = serde_json::Map::new();
    for (kind, path) in &sources {
        let digest = if let Some(path) = path {
            let signature = comparison_signature(path)?;
            let checksum = file_sha256(path)?;
            if comparison_signature(path)? != signature {
                return Err("Input modificato durante la verifica del confronto".into());
            }
            signatures.insert(path.clone(), signature);
            Some(checksum)
        } else {
            None
        };
        digests.insert(kind.clone(), json!(digest));
    }
    let graph = canonical_json_sha256(&json!({"rule":COMPARISON_CACHE_RULE,"raw":digests["raw"]}))?;
    let analysis = canonical_json_sha256(
        &json!({"rule":COMPARISON_CACHE_RULE,"inputs":digests,"categories":rules,"measures":"creation_quality_creators_v3","quality_revision":"selected_official_xml_revision_v1"}),
    )?;
    Ok((
        language,
        ComparisonInput {
            graph,
            analysis,
            sources,
        },
    ))
}

#[cfg(feature = "cli")]
pub(super) fn relative_source(
    root: &std::path::Path,
    path: &std::path::Path,
) -> DumpResult<std::path::PathBuf> {
    let a = root.components().collect::<Vec<_>>();
    let b = path.components().collect::<Vec<_>>();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return Err(
            "Le sorgenti e il work root devono condividere la radice del filesystem".into(),
        );
    }
    let mut result = std::path::PathBuf::new();
    for _ in common..a.len() {
        result.push("..");
    }
    for component in &b[common..] {
        result.push(component.as_os_str());
    }
    Ok(result)
}

/// Verify an immutable cache generation, update a private SQLite backup and
/// publish its hashed manifest last. Graph changes invalidate incident pairs;
/// analysis-only changes retain every completed matching pair and edge.
#[cfg(feature = "cli")]
pub fn compare_files(
    paths: &[std::path::PathBuf],
    options: &ComparisonOptions<'_>,
) -> DumpResult<rusqlite::Connection> {
    crate::runtime::phase("comparison.cache", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::{params, Connection, OpenFlags};
        use serde_json::{json, Value};
        use std::collections::{BTreeMap, BTreeSet};
        if paths.is_empty() || !options.rules.is_object() {
            return Err("Servono raccolte complete e regole valide".into());
        }
        std::fs::create_dir_all(options.work_dir)?;
        let work = resolved_path(options.work_dir)?;
        let cache = work.join("comparison-cache");
        std::fs::create_dir_all(&cache)?;
        let _lock = PublicationLock::acquire_file(&cache.join("writer.lock"))?;
        let manifest = cache.join("manifest.json");
        let original = match std::fs::read(&manifest) {
            Ok(v) => Some(v),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let current = if let Some(bytes) = &original {
            let record: Value = serde_json::from_slice(bytes)?;
            let name = record["file"].as_str().ok_or("Nome cache mancante")?;
            if record["format"] != COMPARISON_CACHE_RULE
                || name.len() != 39
                || !name.ends_with(".sqlite")
                || !name[..32]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("Identità della cache del confronto non valida".into());
            }
            let path = cache.join(name);
            if record["bytes"].as_u64() != Some(file_signature(&path)?.0)
                || record["sha256"] != file_sha256(&path)?
            {
                return Err("Checksum della cache del confronto non valido".into());
            }
            Some(Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?)
        } else {
            None
        };
        let mut previous = BTreeMap::<String, ComparisonInput>::new();
        if let Some(current) = &current {
            let mut query = current.prepare(
                "SELECT language,graph_identity,analysis_identity,sources FROM comparison_inputs",
            )?;
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? {
                let sources: BTreeMap<String, Option<std::path::PathBuf>> =
                    serde_json::from_str(&row.get::<_, String>(3)?)?;
                if sources.values().flatten().any(|p| p.is_absolute()) {
                    return Err("I percorsi della cache devono essere relativi al work root".into());
                }
                previous.insert(
                    row.get(0)?,
                    ComparisonInput {
                        graph: row.get(1)?,
                        analysis: row.get(2)?,
                        sources,
                    },
                );
            }
        }
        let mut supplied = BTreeMap::new();
        for (kind, files) in [
            ("topics", options.topics),
            ("history", options.histories),
            ("quality", options.qualities),
        ] {
            let mut languages = BTreeMap::new();
            for path in files {
                let metadata = first_metadata(path)?;
                let language = metadata["language"]
                    .as_str()
                    .ok_or("Lingua del supplemento mancante")?;
                if languages
                    .insert(language.to_owned(), path.clone())
                    .is_some()
                {
                    return Err("Un solo supplemento per lingua e tipo".into());
                }
            }
            supplied.insert(kind.into(), languages);
        }
        let allowed = |language: &String| {
            options
                .allowed_languages
                .is_none_or(|pool| pool.contains(language))
        };
        let mut inputs = BTreeMap::new();
        let mut signatures = BTreeMap::new();
        for path in paths {
            let (language, input) =
                comparison_source(path, options.rules, &supplied, &mut signatures)?;
            if !allowed(&language) || inputs.insert(language, input).is_some() {
                return Err("Lingua non configurata o raccolta duplicata".into());
            }
        }
        for (language, saved) in &previous {
            if inputs.contains_key(language) || !allowed(language) {
                continue;
            }
            let mut overrides = supplied.clone();
            for kind in ["topics", "history", "quality"] {
                if let Some(path) = saved.sources.get(kind).and_then(Option::as_ref) {
                    overrides
                        .entry(kind.into())
                        .or_default()
                        .entry(language.clone())
                        .or_insert_with(|| work.join(path));
                }
            }
            let raw = saved
                .sources
                .get("raw")
                .and_then(Option::as_ref)
                .ok_or("Sorgente raw della cache mancante")?;
            let (found, input) =
                comparison_source(&work.join(raw), options.rules, &overrides, &mut signatures)?;
            if found != *language {
                return Err("Identità della lingua cambiata nel grafo conservato".into());
            }
            inputs.insert(found, input);
        }
        if inputs.len() < 2 {
            return Err("Servono almeno due raccolte complete di lingue diverse".into());
        }
        let mut saved_sources = BTreeMap::new();
        for (language, input) in &inputs {
            let sources = input
                .sources
                .iter()
                .map(|(kind, path)| {
                    Ok((
                        kind.clone(),
                        path.as_deref()
                            .map(|p| relative_source(&work, p))
                            .transpose()?,
                    ))
                })
                .collect::<DumpResult<BTreeMap<_, _>>>()?;
            saved_sources.insert(language.clone(), sources);
        }
        let dirty = inputs
            .iter()
            .filter(|(l, i)| {
                previous
                    .get(*l)
                    .is_none_or(|p| p.graph != i.graph || p.analysis != i.analysis)
            })
            .map(|(l, _)| l.clone())
            .collect::<BTreeSet<_>>();
        let retired = previous
            .keys()
            .filter(|l| !inputs.contains_key(*l))
            .cloned()
            .collect::<BTreeSet<_>>();
        let paths_changed = inputs.keys().any(|l| {
            previous
                .get(l)
                .is_none_or(|p| p.sources != saved_sources[l])
        });
        if dirty.is_empty() && retired.is_empty() && !paths_changed {
            let current = current.ok_or("Cache mancante")?;
            let languages = current
                .prepare("SELECT language FROM comparison_metadata ORDER BY language")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if languages != inputs.keys().cloned().collect::<Vec<_>>() {
                return Err("Cache del confronto incompleta".into());
            }
            return Ok(current);
        }
        let (prepared, generation) = export_directory(&work)?;
        let staging = prepared.path.join("comparison.sqlite");
        let mut connection = Connection::open(&staging)?;
        if let Some(source) = &current {
            let backup = rusqlite::backup::Backup::new(source, &mut connection)?;
            backup.run_to_completion(4096, std::time::Duration::from_millis(1), None)?;
        } else {
            comparison_schema(&connection)?;
        }
        connection.execute_batch("PRAGMA cache_size=-8192;PRAGMA temp_store=FILE")?;
        let transaction = connection.transaction()?;
        for language in dirty.union(&retired) {
            if retired.contains(language)
                || previous
                    .get(language)
                    .is_none_or(|p| p.graph != inputs[language].graph)
            {
                for table in ["comparison_pairs", "comparison_matches"] {
                    transaction.execute(
                        &format!("DELETE FROM {table} WHERE language=? OR reference=?"),
                        params![language, language],
                    )?;
                }
            }
            for table in [
                "wikis",
                "pages",
                "items",
                "ambiguous",
                "edges",
                "candidates",
                "comparison_inputs",
                "comparison_metadata",
            ] {
                transaction
                    .execute(&format!("DELETE FROM {table} WHERE language=?"), [language])?;
            }
        }
        transaction.commit()?;
        for language in &dirty {
            let input = &inputs[language];
            let raw = input.sources["raw"].as_deref().ok_or("Grafo mancante")?;
            let meta = first_metadata(raw)?;
            let database = crate::wiki_database(language, meta["database"].as_str())?;
            let stage = meta["sql_release"]
                .as_str()
                .map(|r| work.join(format!("{database}-{r}.sqlite")));
            let analysis = AnalysisOptions {
                rules: options.rules,
                work_dir: stage.filter(|p| p.is_file()).map(|_| work.as_path()),
                history: input.sources["history"].as_deref(),
                quality: input.sources["quality"].as_deref(),
                topics: input.sources["topics"].as_deref(),
                batch_size: 512,
            };
            stage_comparison_graph(raw, &mut connection, &analysis)?;
        }
        let transaction = connection.transaction()?;
        for (language, input) in &inputs {
            transaction.execute(
                "INSERT OR REPLACE INTO comparison_inputs VALUES(?,?,?,?)",
                params![
                    language,
                    input.graph,
                    input.analysis,
                    serde_json::to_string(&saved_sources[language])?
                ],
            )?;
        }
        transaction.commit()?;
        if !dirty.is_empty() || !retired.is_empty() {
            if let Some(published) = options.published {
                seed_comparison_pairs(&mut connection, published)?;
            }
            match_comparison(&mut connection)?;
        }
        connection.close().map_err(|(_, e)| e)?;
        for (path, signature) in signatures {
            if comparison_signature(&path)? != signature {
                return Err("Input modificato durante il confronto incrementale".into());
            }
        }
        let name = format!("{generation}.sqlite");
        let checkpoint = json!({"format":COMPARISON_CACHE_RULE,"file":name,"bytes":file_signature(&staging)?.0,"sha256":file_sha256(&staging)?});
        let approved = cache.join(&name);
        if approved.try_exists()? {
            return Err("Generazione della cache già presente".into());
        }
        std::fs::rename(staging, &approved)?;
        DumpClient::<WikimediaHttp>::write_json(&manifest, &checkpoint)?;
        Ok(Connection::open_with_flags(
            approved,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?)
    })
}

/// Adopt completed prior ordered pairs only after verifying their census and
/// every supporting edge against the newly verified graph and unique QIDs.
#[cfg(feature = "cli")]
pub fn seed_comparison_pairs(
    connection: &mut rusqlite::Connection,
    published: &std::collections::BTreeMap<String, std::path::PathBuf>,
) -> DumpResult<()> {
    crate::runtime::phase("comparison.seed", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::{params, OptionalExtension};
        use serde_json::Value;
        use std::collections::{BTreeMap, BTreeSet, HashSet};
        connection.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS edges_by_target ON edges(language,target,source)",
        )?;
        let wikis = connection
            .prepare("SELECT language,metadata FROM wikis")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .map(|r| {
                let (l, m) = r?;
                Ok((l, serde_json::from_str::<Value>(&m)?))
            })
            .collect::<DumpResult<BTreeMap<_, _>>>()?;
        let identities = connection
            .prepare("SELECT language,graph_identity FROM comparison_inputs")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        for (language, path) in published {
            let Some(wiki) = wikis.get(language) else {
                continue;
            };
            let metadata = first_metadata(path)?;
            let comparison = &metadata["comparison"];
            if comparison["rule"] != "cross_language_incoming_v1"
                || comparison["status"] != "compared"
                || metadata["input_sha256"] != wiki["input_sha256"]
            {
                continue;
            }
            let collections = comparison["collections"]
                .as_array()
                .ok_or("Provenienza del confronto precedente mancante")?
                .iter()
                .map(|m| {
                    Ok((
                        m["language"]
                            .as_str()
                            .ok_or("Lingua riferimento mancante")?,
                        m,
                    ))
                })
                .collect::<DumpResult<BTreeMap<_, _>>>()?;
            let mut references = BTreeSet::new();
            for other in comparison["reference_languages"]
                .as_array()
                .ok_or("Lingue di riferimento mancanti")?
            {
                let other = other.as_str().ok_or("Lingua riferimento non valida")?;
                let Some(reference) = wikis.get(other) else {
                    continue;
                };
                let Some(collected) = collections.get(other) else {
                    continue;
                };
                if other != language
                    && ["source", "started_at", "finished_at", "input_sha256"]
                        .iter()
                        .all(|k| collected[*k] == reference[*k])
                    && connection
                        .query_row(
                            "SELECT 1 FROM comparison_pairs WHERE language=? AND reference=?",
                            params![language, other],
                            |r| r.get::<_, i64>(0),
                        )
                        .optional()?
                        .is_none()
                {
                    references.insert(other.to_owned());
                }
            }
            if references.is_empty() {
                continue;
            }
            let staged = stage_export(path)?;
            let transaction = connection.transaction()?;
            transaction.execute_batch("DROP TABLE IF EXISTS temp.seed_seen;DROP TABLE IF EXISTS temp.seed_edges;
            CREATE TEMP TABLE seed_seen(id INTEGER PRIMARY KEY);
            CREATE TEMP TABLE seed_edges(target INTEGER,source INTEGER,reference TEXT,reference_source INTEGER,reference_target INTEGER,title TEXT,outgoing INTEGER,source_title TEXT,target_title TEXT,PRIMARY KEY(reference,target,source)) WITHOUT ROWID")?;
            let mut insert =
                transaction.prepare("INSERT INTO seed_edges VALUES(?,?,?,?,?,?,?,?,?)")?;
            let mut query = staged
                .database
                .prepare("SELECT id,data FROM candidates ORDER BY position")?;
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? {
                let target = row.get::<_, i64>(0)?;
                let record: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
                let candidates = record["candidates"]
                    .as_array()
                    .ok_or("Candidati precedenti mancanti")?;
                let count = if record["status"] == "compared" {
                    Some(candidates.len() as u64)
                } else {
                    None
                };
                if target <= 0
                    || record["candidate_count"] != serde_json::json!(count)
                    || (record["status"] != "compared" && !candidates.is_empty())
                {
                    return Err("Candidati precedenti incompleti".into());
                }
                transaction.execute("INSERT INTO seed_seen VALUES(?)", [target])?;
                let mut seen = HashSet::new();
                for candidate in candidates {
                    let source = candidate["page_id"]
                        .as_i64()
                        .filter(|id| *id > 0)
                        .ok_or("ID candidato non valido")?;
                    let outgoing = candidate["out_degree"]
                        .as_u64()
                        .ok_or("Grado candidato non valido")?;
                    let evidence = candidate["evidence"]
                        .as_array()
                        .filter(|e| !e.is_empty())
                        .ok_or("Candidato precedente senza evidenze")?;
                    if !seen.insert(source) || candidate["dead_end"] != (outgoing == 0) {
                        return Err("Candidato precedente duplicato o incoerente".into());
                    }
                    for edge in evidence {
                        let reference = edge["language"]
                            .as_str()
                            .ok_or("Lingua evidenza mancante")?;
                        if !references.contains(reference) {
                            continue;
                        }
                        insert.execute(params![
                            target,
                            source,
                            reference,
                            edge["source_page_id"]
                                .as_i64()
                                .filter(|id| *id > 0)
                                .ok_or("ID sorgente evidenza non valido")?,
                            edge["target_page_id"]
                                .as_i64()
                                .filter(|id| *id > 0)
                                .ok_or("ID target evidenza non valido")?,
                            candidate["title"]
                                .as_str()
                                .ok_or("Titolo candidato mancante")?,
                            outgoing,
                            edge["source_title"]
                                .as_str()
                                .ok_or("Titolo sorgente mancante")?,
                            edge["target_title"]
                                .as_str()
                                .ok_or("Titolo target mancante")?
                        ])?;
                    }
                }
            }
            drop(rows);
            drop(query);
            drop(insert);
            let count = transaction.query_row(
                "SELECT page_count FROM wikis WHERE language=?",
                [language],
                |r| r.get::<_, usize>(0),
            )?;
            let orphans = transaction.query_row(
                "SELECT COUNT(*) FROM pages WHERE language=? AND incoming=0",
                [language],
                |r| r.get::<_, usize>(0),
            )?;
            if staged.count!=count || transaction.query_row("SELECT COUNT(*) FROM seed_seen",[],|r|r.get::<_,usize>(0))?!=orphans
            || transaction.query_row("SELECT 1 FROM seed_seen s LEFT JOIN pages p ON p.language=? AND p.id=s.id WHERE p.id IS NULL OR p.incoming!=0 LIMIT 1",[language],|r|r.get::<_,i64>(0)).optional()?.is_some() {return Err("Censimento precedente delle orfane incompleto".into());}
            let invalid=transaction.query_row("SELECT 1 FROM seed_edges s
                LEFT JOIN pages lp ON lp.language=? AND lp.id=s.source
                LEFT JOIN pages bp ON bp.language=s.reference AND bp.id=s.reference_source
                LEFT JOIN pages bt ON bt.language=s.reference AND bt.id=s.reference_target
                LEFT JOIN items li ON li.language=? AND li.id=s.source
                LEFT JOIN items si ON si.language=s.reference AND si.id=s.reference_source AND si.item=li.item
                LEFT JOIN items ti ON ti.language=? AND ti.id=s.target
                LEFT JOIN items ri ON ri.language=s.reference AND ri.id=s.reference_target AND ri.item=ti.item
                LEFT JOIN edges e ON e.language=s.reference AND e.target=s.reference_target AND e.source=s.reference_source
                WHERE lp.id IS NULL OR bp.id IS NULL OR bt.id IS NULL OR si.id IS NULL OR ri.id IS NULL OR e.source IS NULL OR s.target=s.source OR lp.title!=s.title OR lp.outgoing!=s.outgoing OR bp.title!=s.source_title OR bt.title!=s.target_title LIMIT 1",params![language,language,language],|r|r.get::<_,i64>(0)).optional()?.is_some();
            if invalid || file_signature(path)? != staged.signature {
                return Err("Evidenze precedenti incoerenti con i grafi verificati".into());
            }
            transaction.execute("INSERT INTO comparison_matches SELECT ?,reference,target,source,reference_source,reference_target FROM seed_edges",[language])?;
            let identity = identities.get(language).ok_or("Identità locale mancante")?;
            for other in references {
                transaction.execute(
                    "INSERT INTO comparison_pairs VALUES(?,?,?,?)",
                    params![
                        language,
                        other,
                        identity,
                        identities
                            .get(&other)
                            .ok_or("Identità riferimento mancante")?
                    ],
                )?;
            }
            transaction.execute_batch("DROP TABLE temp.seed_seen;DROP TABLE temp.seed_edges")?;
            transaction.commit()?;
        }
        Ok(())
    })
}

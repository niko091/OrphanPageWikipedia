use super::DumpResult;
#[cfg(feature = "cli")]
use super::{
    analyze_imported_graph, analyze_memory_graph, export_directory, file_signature, first_metadata,
    resolved_path, sql_redirects, stage_page_batches, visit_ordered_json, write_atomic,
    AnalysisOptions, ImportedGraph,
};

/// Create the same normalized on-disk comparison indices used by the frozen
/// implementation. Complete ordered pairs include pairs with no evidence.
#[cfg(feature = "cli")]
pub fn comparison_schema(connection: &rusqlite::Connection) -> DumpResult<()> {
    connection.execute_batch("PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;
        CREATE TABLE wikis(language TEXT PRIMARY KEY, metadata TEXT, page_count INTEGER);
        CREATE TABLE pages(language TEXT, id INTEGER, position INTEGER, item TEXT, title TEXT,
            incoming INTEGER, outgoing INTEGER, data TEXT, PRIMARY KEY(language,id)) WITHOUT ROWID;
        CREATE INDEX pages_by_position ON pages(language,position);
        CREATE TABLE items(language TEXT, item TEXT, id INTEGER,
            PRIMARY KEY(language,item), UNIQUE(language,id)) WITHOUT ROWID;
        CREATE INDEX items_by_item ON items(item,language,id);
        CREATE TABLE ambiguous(language TEXT, item TEXT, PRIMARY KEY(language,item)) WITHOUT ROWID;
        CREATE TABLE edges(language TEXT, target INTEGER, source INTEGER);
        CREATE TABLE candidates(language TEXT, id INTEGER, status TEXT, candidate_count INTEGER, data TEXT,
            PRIMARY KEY(language,id)) WITHOUT ROWID;
        CREATE TABLE comparison_inputs(language TEXT PRIMARY KEY, graph_identity TEXT,
            analysis_identity TEXT, sources TEXT);
        CREATE TABLE comparison_metadata(language TEXT PRIMARY KEY, metadata TEXT);
        CREATE TABLE comparison_pairs(language TEXT, reference TEXT, local_identity TEXT,
            reference_identity TEXT, PRIMARY KEY(language,reference)) WITHOUT ROWID;
        CREATE TABLE comparison_matches(language TEXT, reference TEXT, target INTEGER, source INTEGER,
            reference_source INTEGER, reference_target INTEGER,
            PRIMARY KEY(language,reference,target,source)) WITHOUT ROWID;
        CREATE INDEX matches_by_target ON comparison_matches(language,target,source,reference);")?;
    Ok(())
}

#[cfg(feature = "cli")]
pub(super) fn approved_graph(meta: &serde_json::Value, items: bool) -> DumpResult<()> {
    let language = meta["language"].as_str().ok_or("Lingua mancante")?;
    let database = crate::wiki_database(language, meta["database"].as_str())?;
    if meta["complete"] != true
        || (meta["demo"] != true
            && (meta["source"] != format!("Wikimedia SQL dumps / {database}")
                || !meta["sql_release"].as_str().is_some_and(|s| {
                    s.len() == 8 && s.starts_with("20") && s.bytes().all(|b| b.is_ascii_digit())
                })
                || !meta["dump_manifest_sha256"].as_str().is_some_and(|s| {
                    s.len() == 64
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })))
    {
        return Err("Servono esclusivamente censimenti SQL completi verificati".into());
    }
    if items
        && meta["item_mapping"]
            != serde_json::json!({"source":"page_props.wikibase_item","complete":true})
    {
        return Err("Identificativi interlingua non raccolti".into());
    }
    Ok(())
}

/// Stage one edition's observations and only its QID-eligible resolved edges.
/// Large SQL graphs retain no full adjacency and release their indices before
/// the next edition. Scientific degrees still count every article and edge.
#[cfg(feature = "cli")]
pub fn stage_comparison_graph(
    raw: &std::path::Path,
    connection: &mut rusqlite::Connection,
    options: &AnalysisOptions<'_>,
) -> DumpResult<()> {
    crate::runtime::phase("comparison.graph_index", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::params;
        use serde_json::{json, Value};
        use std::io::{BufReader, BufWriter, Write};
        if !(1..=512).contains(&options.batch_size) {
            return Err("Dimensione blocco non valida".into());
        }
        let signature = file_signature(raw)?;
        let meta = first_metadata(raw)?;
        approved_graph(&meta, true)?;
        let language = meta["language"].as_str().ok_or("Lingua mancante")?;
        let bounded = signature.0 >= 128 * 1024 * 1024
            && meta["source"]
                .as_str()
                .is_some_and(|s| s.starts_with("Wikimedia SQL dumps / "));
        let read = |p: &std::path::Path| -> DumpResult<Value> {
            Ok(serde_json::from_reader(BufReader::new(
                std::fs::File::open(p)?,
            ))?)
        };
        let (prepared, _) = export_directory(&std::env::temp_dir())?;
        let canonical = prepared.path.join("analysis.json");
        let graph = if bounded {
            analyze_imported_graph(raw, &canonical, options)?;
            None
        } else {
            let graph = read(raw)?;
            let history = options.history.map(read).transpose()?;
            let quality = options.quality.map(read).transpose()?;
            let topics = options.topics.map(read).transpose()?;
            let result = analyze_memory_graph(
                &graph,
                options.rules,
                history.as_ref(),
                quality.as_ref(),
                topics.as_ref(),
                false,
            )?;
            write_atomic(&canonical, |file| {
                let mut writer = BufWriter::new(file);
                serde_json::to_writer(&mut writer, &result)?;
                writer.flush()?;
                Ok(())
            })?;
            Some(graph)
        };
        let transaction = connection.transaction()?;
        let mut count = 0usize;
        let mut insert = transaction.prepare("INSERT INTO pages VALUES(?,?,?,?,?,?,?,?)")?;
        let mut metadata = None;
        visit_ordered_json(
            BufReader::new(std::fs::File::open(&canonical)?),
            false,
            false,
            &mut |section, _, record| {
                if section == "metadata" {
                    metadata = Some(record);
                } else {
                    let id = record["page_id"]
                        .as_i64()
                        .filter(|id| *id > 0)
                        .ok_or("ID pagina non valido")?;
                    let incoming = record["in_degree"]
                        .as_u64()
                        .ok_or("Grado entrante mancante")?;
                    let outgoing = record["out_degree"]
                        .as_u64()
                        .ok_or("Grado uscente mancante")?;
                    insert.execute(params![
                        language,
                        id,
                        count,
                        record["wikibase_item"].as_str(),
                        record["title"].as_str().ok_or("Titolo mancante")?,
                        incoming,
                        outgoing,
                        serde_json::to_string(&record)?
                    ])?;
                    count += 1;
                }
                Ok(())
            },
        )?;
        drop(insert);
        transaction.execute(
            "INSERT INTO wikis VALUES(?,?,?)",
            params![
                language,
                serde_json::to_string(&metadata.ok_or("Metadati mancanti")?)?,
                count
            ],
        )?;
        let mut query = transaction
            .prepare("SELECT id,title,item FROM pages WHERE language=? ORDER BY position")?;
        let records = query
            .query_map([language], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(query);
        let ids = records.iter().map(|r| r.0).collect::<Vec<_>>();
        let (mapping, ambiguous) =
            crate::unique_items(&records.iter().map(|r| r.2.clone()).collect::<Vec<_>>());
        for (item, position) in &mapping {
            transaction.execute(
                "INSERT INTO items VALUES(?,?,?)",
                params![language, item, ids[*position]],
            )?;
        }
        for item in ambiguous {
            transaction.execute("INSERT INTO ambiguous VALUES(?,?)", params![language, item])?;
        }
        let imported = if bounded {
            Some(ImportedGraph::open(raw, options.work_dir, true)?)
        } else {
            None
        };
        let redirects = if let Some(imported) = &imported {
            sql_redirects(&imported.source)?
        } else {
            serde_json::from_value(
                graph
                    .as_ref()
                    .ok_or("Grafo mancante")?
                    .get("redirects")
                    .cloned()
                    .unwrap_or(json!({})),
            )?
        };
        let mut accumulator = crate::GraphAccumulator::new(
            records.into_iter().map(|r| r.1).collect(),
            redirects,
            Some(mapping.into_values().collect()),
        )?;
        let mut insert = transaction.prepare("INSERT INTO edges VALUES(?,?,?)")?;
        let mut position = 0;
        let mut add = |batch: Vec<(i64, Vec<String>)>| -> DumpResult<()> {
            let mut links = Vec::with_capacity(batch.len());
            for (id, targets) in batch {
                if ids.get(position) != Some(&id) {
                    return Err("Ordine del censimento cambiato".into());
                }
                position += 1;
                links.push(targets);
            }
            for (target, source) in accumulator.add_batch_edges(links)? {
                insert.execute(params![language, ids[target], ids[source]])?;
            }
            Ok(())
        };
        if let Some(imported) = &imported {
            stage_page_batches(&imported.source, options.batch_size, true, |batch| {
                add(batch.into_iter().map(|p| (p.id, p.links)).collect())
            })?;
        } else {
            for batch in graph.as_ref().ok_or("Grafo mancante")?["pages"]
                .as_array()
                .ok_or("Pagine mancanti")?
                .chunks(options.batch_size)
            {
                add(batch
                    .iter()
                    .map(|p| {
                        Ok((
                            p["page_id"].as_i64().ok_or("ID mancante")?,
                            serde_json::from_value(p.get("links").cloned().unwrap_or(json!([])))?,
                        ))
                    })
                    .collect::<DumpResult<Vec<_>>>()?)?;
            }
        }
        drop(insert);
        let (incoming, outgoing, _) = accumulator.finish()?;
        let mut query = transaction
            .prepare("SELECT id,incoming,outgoing FROM pages WHERE language=? ORDER BY position")?;
        let mut rows = query.query([language])?;
        for ((id, i), o) in ids.iter().zip(incoming).zip(outgoing) {
            let row = rows.next()?.ok_or("Censimento incompleto")?;
            if row.get::<_, i64>(0)? != *id
                || row.get::<_, u64>(1)? != i
                || row.get::<_, u64>(2)? != o
            {
                return Err("Gradi del grafo incoerenti".into());
            }
        }
        if position != count || file_signature(raw)? != signature {
            return Err("Grafo incompleto o modificato".into());
        }
        drop(rows);
        drop(query);
        transaction.commit()?;
        Ok(())
    })
}

#[cfg(feature = "cli")]
pub(super) fn comparison_edge_query(ids: &str, references: &str) -> String {
    format!(
        "SELECT p.id,lp.id,lp.title,lp.outgoing,ti.language,bp.id,bp.title,bt.id,bt.title
        FROM pages p
        CROSS JOIN items ti ON ti.item=p.item AND ti.language!=p.language
        CROSS JOIN edges e ON e.language=ti.language AND e.target=ti.id
        CROSS JOIN items si ON si.language=e.language AND si.id=e.source
        CROSS JOIN items li ON li.language=p.language AND li.item=si.item
        CROSS JOIN pages lp ON lp.language=li.language AND lp.id=li.id
        CROSS JOIN pages bp ON bp.language=si.language AND bp.id=si.id
        CROSS JOIN pages bt ON bt.language=ti.language AND bt.id=ti.id
        WHERE p.language=? AND p.id IN ({ids}) AND li.id!=p.id {references}
        ORDER BY p.id,lp.outgoing!=0,lp.title,lp.id,ti.language"
    )
}

#[cfg(feature = "cli")]
pub(super) fn comparison_blocks(
    connection: &rusqlite::Connection,
    language: &str,
    eligible_only: bool,
    mut consume: impl FnMut(&[i64]) -> DumpResult<usize>,
) -> DumpResult<()> {
    let sql = if eligible_only {
        "SELECT p.id FROM pages p JOIN items i ON i.language=p.language AND i.id=p.id WHERE p.language=? AND p.incoming=0 AND p.id>? ORDER BY p.id LIMIT ?"
    } else {
        "SELECT id FROM pages WHERE language=? AND incoming=0 AND id>? ORDER BY id LIMIT ?"
    };
    let mut query = connection.prepare(sql)?;
    let mut after = 0i64;
    let mut size = 512usize;
    loop {
        crate::runtime::check_cancelled()?;
        let ids = query
            .query_map(rusqlite::params![language, after, size], |r| {
                r.get::<_, i64>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let Some(last) = ids.last() else { break };
        let rows = crate::runtime::phase("comparison.block", || -> DumpResult<usize> {
            crate::runtime::phase_tag("language", serde_json::json!(language));
            let rows = consume(&ids)?;
            crate::runtime::phase_counts(Some(rows as u64), None);
            Ok(rows)
        })?;
        size = (ids.len().saturating_mul(8192) / rows.max(1)).clamp(1, 512);
        after = *last;
    }
    Ok(())
}

/// Compute only missing ordered pairs, then stream sorted evidence into one
/// final target record. No target, candidate or reference edge is truncated.
#[cfg(feature = "cli")]
pub fn match_comparison(connection: &mut rusqlite::Connection) -> DumpResult<()> {
    crate::runtime::phase("comparison.match", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::{params, OptionalExtension};
        use serde_json::{json, Value};
        use std::collections::BTreeMap;
        connection.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS edges_by_target ON edges(language,target,source)",
        )?;
        let wikis = connection
            .prepare("SELECT language,metadata,page_count FROM wikis ORDER BY language")?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, usize>(2)?,
                ))
            })?
            .map(|r| {
                let (lang, meta, count) = r?;
                Ok((lang, serde_json::from_str::<Value>(&meta)?, count))
            })
            .collect::<DumpResult<Vec<_>>>()?;
        if wikis.len() < 2
            || wikis
                .iter()
                .any(|(_, m, _)| (m["demo"] == true) != (wikis[0].1["demo"] == true))
        {
            return Err("Servono almeno due censimenti omogenei di lingue diverse".into());
        }
        let identities = connection
            .prepare("SELECT language,graph_identity FROM comparison_inputs ORDER BY language")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        if identities.len() != wikis.len()
            || wikis.iter().any(|(l, _, _)| !identities.contains_key(l))
        {
            return Err("Identità della cache incomplete".into());
        }
        for (language, identity) in &identities {
            let mut references = Vec::new();
            for (other, reference) in &identities {
                if other!=language && connection.query_row("SELECT local_identity,reference_identity FROM comparison_pairs WHERE language=? AND reference=?",params![language,other],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?.as_ref()!=Some(&(identity.clone(),reference.clone())) {references.push(other.as_str());}
            }
            if references.is_empty() {
                continue;
            }
            let transaction = connection.transaction()?;
            for other in &references {
                transaction.execute(
                    "DELETE FROM comparison_matches WHERE language=? AND reference=?",
                    params![language, other],
                )?;
                transaction.execute(
                    "DELETE FROM comparison_pairs WHERE language=? AND reference=?",
                    params![language, other],
                )?;
            }
            let placeholders = std::iter::repeat_n("?", references.len())
                .collect::<Vec<_>>()
                .join(",");
            let filter = format!(" AND ti.language IN ({placeholders})");
            let bindings = std::iter::once(language.as_str())
                .chain(references.iter().copied())
                .collect::<Vec<_>>();
            let mut insert =
                transaction.prepare("INSERT INTO comparison_matches VALUES(?,?,?,?,?,?)")?;
            comparison_blocks(&transaction, language, true, |ids| {
                let ids = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
                let mut query = transaction.prepare(&comparison_edge_query(&ids, &filter))?;
                let mut rows = query.query(rusqlite::params_from_iter(bindings.iter()))?;
                let mut count = 0;
                while let Some(row) = rows.next()? {
                    insert.execute(params![
                        language,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(7)?
                    ])?;
                    count += 1;
                }
                Ok(count)
            })?;
            drop(insert);
            for other in references {
                transaction.execute(
                    "INSERT INTO comparison_pairs VALUES(?,?,?,?)",
                    params![language, other, identity, identities[other]],
                )?;
            }
            transaction.commit()?;
        }
        let provenance = wikis
            .iter()
            .map(|(_, m, _)| {
                [
                    "language",
                    "source",
                    "started_at",
                    "finished_at",
                    "input_sha256",
                ]
                .map(|k| (k.to_owned(), m[k].clone()))
                .into_iter()
                .collect::<serde_json::Map<_, _>>()
            })
            .collect::<Vec<_>>();
        let counts = |table: &str| -> DumpResult<Value> {
            Ok(wikis
                .iter()
                .map(|(l, _, _)| {
                    Ok((
                        l.clone(),
                        json!(connection.query_row(
                            &format!("SELECT COUNT(*) FROM {table} WHERE language=?"),
                            [l],
                            |r| r.get::<_, u64>(0)
                        )?),
                    ))
                })
                .collect::<DumpResult<serde_json::Map<_, _>>>()?
                .into())
        };
        let mapped = counts("items")?;
        let ambiguous = counts("ambiguous")?;
        let transaction = connection.transaction()?;
        transaction.execute_batch("DELETE FROM candidates;DELETE FROM comparison_metadata")?;
        for (language, mut metadata, _) in wikis.iter().cloned() {
            comparison_blocks(&transaction, &language, false, |block| {
                let ids = block
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let mut prepared = BTreeMap::<i64, (&str, Vec<Value>)>::new();
                let mut query=transaction.prepare(&format!("SELECT p.id,i.id,a.item FROM pages p LEFT JOIN items i ON i.language=p.language AND i.item=p.item LEFT JOIN ambiguous a ON a.language=p.language AND a.item=p.item WHERE p.language=? AND p.id IN ({ids})"))?;
                let mut rows = query.query([&language])?;
                while let Some(r) = rows.next()? {
                    prepared.insert(
                        r.get(0)?,
                        (
                            if r.get::<_, Option<String>>(2)?.is_some() {
                                "ambiguous_item"
                            } else if r.get::<_, Option<i64>>(1)?.is_none() {
                                "missing_item"
                            } else {
                                "compared"
                            },
                            Vec::new(),
                        ),
                    );
                }
                let mut query=transaction.prepare(&format!("SELECT p.id,w.language,cp.id,cp.title,cp.incoming,a.item FROM pages p JOIN items local ON local.language=p.language AND local.item=p.item CROSS JOIN wikis w LEFT JOIN items i ON i.language=w.language AND i.item=p.item LEFT JOIN pages cp ON cp.language=i.language AND cp.id=i.id LEFT JOIN ambiguous a ON a.language=w.language AND a.item=p.item WHERE p.language=? AND p.id IN ({ids}) AND w.language!=? ORDER BY p.id,w.language"))?;
                let mut rows = query.query(params![language, language])?;
                while let Some(r) = rows.next()? {
                    let other = r.get::<_, String>(1)?;
                    let counterpart = if let Some(id) = r.get::<_, Option<i64>>(2)? {
                        let incoming = r.get::<_, u64>(4)?;
                        json!({"language":other,"status":if incoming==0 {"orphan"}else{"non_orphan"},"page_id":id,"title":r.get::<_,String>(3)?,"in_degree":incoming})
                    } else {
                        json!({"language":other,"status":if r.get::<_,Option<String>>(5)?.is_some(){"ambiguous_item"}else{"missing_counterpart"}})
                    };
                    prepared
                        .get_mut(&r.get::<_, i64>(0)?)
                        .ok_or("Orfana non indicizzata")?
                        .1
                        .push(counterpart);
                }
                let mut query=transaction.prepare(&format!("SELECT m.target,lp.id,lp.title,lp.outgoing,m.reference,bp.id,bp.title,bt.id,bt.title FROM comparison_matches m CROSS JOIN pages lp ON lp.language=m.language AND lp.id=m.source CROSS JOIN pages bp ON bp.language=m.reference AND bp.id=m.reference_source CROSS JOIN pages bt ON bt.language=m.reference AND bt.id=m.reference_target WHERE m.language=? AND m.target IN ({ids}) ORDER BY m.target,lp.outgoing!=0,lp.title,lp.id,m.reference"))?;
                let mut rows = query.query([&language])?;
                let read =
                    |row: &rusqlite::Row<'_>| -> rusqlite::Result<(i64, crate::CandidateEdge)> {
                        Ok((
                            row.get(0)?,
                            (
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                                row.get(7)?,
                                row.get(8)?,
                            ),
                        ))
                    };
                let mut current = rows.next()?.map(read).transpose()?;
                let mut edge_count = 0;
                for id in block {
                    let (mut status, counterparts) =
                        prepared.remove(id).ok_or("Orfana non indicizzata")?;
                    let mut candidates = Vec::<Value>::new();
                    while current.as_ref().is_some_and(|r| r.0 == *id) {
                        let (_, edge) = current.take().ok_or("Evidenza mancante")?;
                        let (
                            source,
                            title,
                            outgoing,
                            reference,
                            reference_source,
                            source_title,
                            reference_target,
                            target_title,
                        ) = edge;
                        if status != "compared"
                            || source <= 0
                            || title.is_empty()
                            || reference.is_empty()
                            || reference_source <= 0
                            || source_title.is_empty()
                            || reference_target <= 0
                            || target_title.is_empty()
                        {
                            return Err("Evidenza candidata non valida".into());
                        }
                        let evidence = json!({"language":reference,"source_page_id":reference_source,"source_title":source_title,"target_page_id":reference_target,"target_title":target_title});
                        if let Some(candidate) =
                            candidates.last_mut().filter(|c| c["page_id"] == source)
                        {
                            candidate["evidence"]
                                .as_array_mut()
                                .ok_or("Evidenze mancanti")?
                                .push(evidence);
                        } else {
                            candidates.push(json!({"page_id":source,"title":title,"out_degree":outgoing,"dead_end":outgoing==0,"evidence":[evidence]}));
                        }
                        edge_count += 1;
                        current = rows.next()?.map(read).transpose()?;
                    }
                    if status == "compared"
                        && !counterparts
                            .iter()
                            .any(|c| c["status"] == "orphan" || c["status"] == "non_orphan")
                    {
                        status = "counterpart_unavailable";
                    }
                    let count = if status == "compared" {
                        Some(candidates.len())
                    } else {
                        None
                    };
                    let record = json!({"status":status,"candidate_count":count,"counterparts":counterparts,"candidates":candidates});
                    transaction.execute(
                        "INSERT INTO candidates VALUES(?,?,?,?,?)",
                        params![language, id, status, count, serde_json::to_string(&record)?],
                    )?;
                }
                if current.is_some() {
                    return Err("Evidenza di target inatteso".into());
                }
                Ok(edge_count)
            })?;
            metadata["comparison"] = json!({"rule":"cross_language_incoming_v1","status":"compared","reference_languages":wikis.iter().filter(|w|w.0!=language).map(|w|&w.0).collect::<Vec<_>>(),"collections":provenance,"matching_source":"local page_props.wikibase_item from SQL dumps","ambiguous_items_excluded":ambiguous,"mapped_articles":mapped,"temporal_consistency":"independent_source_specific_intervals","interpretation":"candidates for manual review, not validated recommendations"});
            transaction.execute(
                "INSERT INTO comparison_metadata VALUES(?,?)",
                params![language, serde_json::to_string(&metadata)?],
            )?;
        }
        transaction.commit()?;
        Ok(())
    })
}

/// Stream one completed comparison member from the normalized indices. The
/// cursor retains a single page or complete final target record at a time.
#[cfg(feature = "cli")]
pub fn export_comparison_member(
    connection: &rusqlite::Connection,
    language: &str,
    output: &std::path::Path,
) -> DumpResult<()> {
    crate::runtime::phase("comparison.member", || {
        crate::runtime::check_cancelled()?;
        use std::io::{BufWriter, Write};
        if let Some(database) = connection.path().filter(|p| !p.is_empty()) {
            if resolved_path(output)? == resolved_path(std::path::Path::new(database))? {
                return Err("Output sovrapposto alla cache".into());
            }
        }
        let (encoded,expected)=connection.query_row("SELECT m.metadata,w.page_count FROM wikis w JOIN comparison_metadata m ON m.language=w.language WHERE w.language=?",[language],|r|Ok((r.get::<_,String>(0)?,r.get::<_,usize>(1)?)))?;
        let metadata: serde_json::Value = serde_json::from_str(&encoded)?;
        if metadata["language"] != language
            || metadata["complete"] != true
            || metadata["comparison"]["status"] != "compared"
        {
            return Err("Confronto incompleto".into());
        }
        write_atomic(output, |file| {
            let mut writer = BufWriter::with_capacity(65536, file);
            writer.write_all(b"{\"metadata\":")?;
            serde_json::to_writer(&mut writer, &metadata)?;
            writer.write_all(b",\"pages\":[")?;
            let mut query =
                connection.prepare("SELECT data FROM pages WHERE language=? ORDER BY position")?;
            let mut rows = query.query([language])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                if count > 0 {
                    writer.write_all(b",")?;
                }
                writer.write_all(row.get::<_, String>(0)?.as_bytes())?;
                count += 1;
            }
            if count != expected {
                return Err("Censimento del confronto incompleto".into());
            }
            writer.write_all(b"],\"link_candidates\":{")?;
            let mut query = connection
                .prepare("SELECT id,data FROM candidates WHERE language=? ORDER BY id")?;
            let mut rows = query.query([language])?;
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
    })
}

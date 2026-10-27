//! One immutable, relational publication for the complete selected census.
//! JSON is an HTTP encoding and a lossless encoding of scientific attributes,
//! not a second on-disk publication. Reviews live in their independent store.
use crate::dumps::{self, DumpResult};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs::File, io::BufReader, path::Path};

pub const RULE: &str = "relational_census_v1";
const PAGE_SIZE: i64 = 20;

fn read(path: &Path) -> DumpResult<Value> {
    Ok(serde_json::from_reader(BufReader::new(File::open(path)?))?)
}

fn filename(value: &Value) -> DumpResult<&str> {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && *s != "."
                && *s != ".."
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
        .ok_or_else(|| "Nome della risorsa non valido".into())
}

fn encode(value: &Value) -> DumpResult<String> {
    Ok(serde_json::to_string(value)?)
}

fn intern(db: &Connection, value: &str) -> DumpResult<i64> {
    db.prepare_cached("INSERT OR IGNORE INTO texts(value) VALUES(?)")?
        .execute([value])?;
    Ok(db
        .prepare_cached("SELECT id FROM texts WHERE value=?")?
        .query_row([value], |r| r.get(0))?)
}

fn decode_page(data: String, id: i64, title: String, orphan: bool) -> DumpResult<Value> {
    let mut page: Value = serde_json::from_str(&data)?;
    page["page_id"] = json!(id);
    page["title"] = json!(title);
    page["orphan"] = json!(orphan);
    Ok(page)
}

fn fingerprint(db: &Connection) -> DumpResult<String> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for (table, order) in [
        ("datasets", "language"),
        ("documents", "path"),
        ("texts", "id"),
        ("provenance", "language,kind"),
        ("pages", "language,id"),
        ("topics", "language,page_id,label"),
        ("targets", "language,id"),
        ("candidates", "language,target,source"),
        ("evidence", "language,target,source,position"),
        ("shortlists", "language,target,source"),
        ("creators", "dataset,key"),
        ("publication", "key"),
    ] {
        hash.update((table.len() as u64).to_le_bytes());
        hash.update(table.as_bytes());
        let exclude = if table == "publication" {
            " WHERE key!='integrity_sha256'"
        } else {
            ""
        };
        let mut query = db.prepare(&format!("SELECT * FROM {table}{exclude} ORDER BY {order}"))?;
        let columns = query.column_count();
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            crate::runtime::check_cancelled()?;
            hash.update([255]);
            for column in 0..columns {
                use rusqlite::types::ValueRef;
                match row.get_ref(column)? {
                    ValueRef::Null => hash.update([0]),
                    ValueRef::Integer(n) => {
                        hash.update([1]);
                        hash.update(n.to_le_bytes());
                    }
                    ValueRef::Real(n) => {
                        hash.update([2]);
                        hash.update(n.to_le_bytes());
                    }
                    ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
                        hash.update([3]);
                        hash.update((bytes.len() as u64).to_le_bytes());
                        hash.update(bytes);
                    }
                }
            }
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Atomically replace the publication only after every member and all evidence
/// have been imported and verified. Neither source exports nor the old DB change
/// on failure. Only a fresh, privately owned temporary DB can be removed here.
pub fn publish(folder: &Path, destination: &Path, identity: &Value) -> DumpResult<()> {
    if destination.extension().and_then(|s| s.to_str()) != Some("sqlite") {
        return Err("Il database deve avere estensione .sqlite".into());
    }
    let entries = read(&folder.join("manifest.json"))?;
    let entries = entries
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("Manifest mancante")?;
    // Select one latest member per edition; historical exports are migration
    // inputs only, never additional releases in the published database.
    let mut latest = std::collections::BTreeMap::<String, Value>::new();
    for entry in entries.iter().filter(|e| e["kind"] != "aggregate") {
        let language = entry["language"]
            .as_str()
            .ok_or("Lingua mancante")?
            .to_owned();
        let rank = |e: &Value| {
            (
                e["date"].as_str().unwrap_or("").to_owned(),
                e["published_at"].as_str().unwrap_or("").to_owned(),
                e["file"].as_str().unwrap_or("").to_owned(),
            )
        };
        if latest
            .get(&language)
            .is_none_or(|old| rank(entry) > rank(old))
        {
            latest.insert(language, entry.clone());
        }
    }
    let mut selected = latest.into_values().collect::<Vec<_>>();
    if selected.len() > 1 {
        let mut aggregates = entries
            .iter()
            .filter(|e| e["kind"] == "aggregate")
            .collect::<Vec<_>>();
        aggregates.reverse();
        let aggregate = aggregates
            .into_iter()
            .find(|entry| {
                read(&folder.join(entry["file"].as_str().unwrap_or("")))
                    .ok()
                    .is_some_and(|index| {
                        index["members"].as_array().is_some_and(|members| {
                            members.len() == selected.len()
                                && selected.iter().all(|e| {
                                    members.iter().any(|m| {
                                        m["file"] == e["file"] && m["language"] == e["language"]
                                    })
                                })
                        })
                    })
            })
            .ok_or("Aggregato della raccolta recente mancante")?;
        selected.push(aggregate.clone());
    }
    let entries = &selected;
    let parent = destination
        .parent()
        .ok_or("Directory del database mancante")?;
    std::fs::create_dir_all(parent)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(destination.with_extension("publish.lock"))?;
    lock.try_lock()
        .map_err(|_| "Un'altra pubblicazione sta usando il database")?;
    let temporary = parent.join(format!(
        ".collection-{}.sqlite",
        crate::runtime::unique_id()
    ));
    let result = (|| -> DumpResult<()> {
        let mut db = Connection::open(&temporary)?;
        db.execute_batch("PRAGMA page_size=4096; PRAGMA cache_size=-16384; PRAGMA temp_store=FILE;
            PRAGMA foreign_keys=ON;
            CREATE TABLE publication(key TEXT PRIMARY KEY, data TEXT NOT NULL) WITHOUT ROWID;
            CREATE TABLE datasets(language TEXT PRIMARY KEY, file TEXT NOT NULL UNIQUE, entry TEXT NOT NULL,
                metadata TEXT NOT NULL, page_count INTEGER NOT NULL) WITHOUT ROWID;
            CREATE TABLE documents(path TEXT PRIMARY KEY,data TEXT NOT NULL) WITHOUT ROWID;
            CREATE TABLE texts(id INTEGER PRIMARY KEY,value TEXT NOT NULL UNIQUE);
            CREATE TABLE provenance(language TEXT NOT NULL,kind TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(language,kind)) WITHOUT ROWID;
            CREATE TABLE pages(language TEXT NOT NULL,id INTEGER NOT NULL,position INTEGER NOT NULL,title TEXT NOT NULL,title_fold TEXT NOT NULL,
                orphan INTEGER NOT NULL,category TEXT,category_fold TEXT,special INTEGER NOT NULL,redirect INTEGER NOT NULL,
                length INTEGER,created INTEGER,topic_status TEXT,data TEXT NOT NULL,
                PRIMARY KEY(language,id),FOREIGN KEY(language) REFERENCES datasets(language)) WITHOUT ROWID;
            CREATE TABLE topics(language TEXT NOT NULL,page_id INTEGER NOT NULL,label TEXT NOT NULL,
                PRIMARY KEY(language,page_id,label),FOREIGN KEY(language,page_id) REFERENCES pages(language,id)) WITHOUT ROWID;
            CREATE TABLE targets(language TEXT NOT NULL,id INTEGER NOT NULL,count INTEGER,status TEXT,data TEXT NOT NULL,
                PRIMARY KEY(language,id),FOREIGN KEY(language,id) REFERENCES pages(language,id)) WITHOUT ROWID;
            CREATE TABLE candidates(language TEXT NOT NULL,target INTEGER NOT NULL,source INTEGER NOT NULL,position INTEGER NOT NULL,title_id INTEGER NOT NULL REFERENCES texts(id),data TEXT NOT NULL,
                PRIMARY KEY(language,target,source),UNIQUE(language,target,position),
                FOREIGN KEY(language,target) REFERENCES targets(language,id),
                FOREIGN KEY(language,source) REFERENCES pages(language,id)) WITHOUT ROWID;
            CREATE TABLE evidence(language TEXT NOT NULL,target INTEGER NOT NULL,source INTEGER NOT NULL,position INTEGER NOT NULL,
                reference_language TEXT NOT NULL,source_title_id INTEGER NOT NULL REFERENCES texts(id),target_title_id INTEGER NOT NULL REFERENCES texts(id),data TEXT NOT NULL,PRIMARY KEY(language,target,source,position),
                FOREIGN KEY(language,target,source) REFERENCES candidates(language,target,source)) WITHOUT ROWID;
            CREATE TABLE shortlists(language TEXT NOT NULL,target INTEGER NOT NULL,source INTEGER NOT NULL,position INTEGER NOT NULL,evidence_position INTEGER NOT NULL,
                PRIMARY KEY(language,target,source),FOREIGN KEY(language,target,source) REFERENCES candidates(language,target,source)) WITHOUT ROWID;
            CREATE TABLE creators(dataset TEXT NOT NULL,key TEXT NOT NULL,key_fold TEXT NOT NULL,articles INTEGER,total INTEGER NOT NULL,
                orphans INTEGER NOT NULL,data TEXT NOT NULL,PRIMARY KEY(dataset,key)) WITHOUT ROWID;
            CREATE INDEX page_titles ON pages(language,title_fold,id);
            CREATE INDEX page_groups ON pages(language,orphan,title_fold,id);
            CREATE INDEX page_lengths ON pages(language,orphan,length,title_fold,id);
            CREATE INDEX page_dates ON pages(language,orphan,created,title_fold,id);
            CREATE INDEX page_categories ON pages(language,category_fold,id);
            CREATE INDEX topic_labels ON topics(label,language,page_id);
            CREATE INDEX target_counts ON targets(language,count,id);
            CREATE INDEX creator_orphans ON creators(dataset,orphans,key);
            CREATE INDEX creator_articles ON creators(dataset,articles,key);
            CREATE INDEX creator_totals ON creators(dataset,total,key);")?;
        let tx = db.transaction()?;
        let mut manifest = Vec::new();
        let mut languages = BTreeSet::new();
        let mut releases = None;
        let mut signatures = Vec::new();
        for entry in entries.iter().filter(|e| e["kind"] != "aggregate") {
            let file = filename(&entry["file"])?;
            let path = folder.join(file);
            let source = dumps::stage_export(&path)?;
            let meta = &source.metadata;
            let language = meta["language"].as_str().ok_or("Lingua mancante")?;
            if entry["language"] != language
                || meta["finished_at"] != entry["date"]
                || !languages.insert(language.to_owned())
                || meta["complete"] != true
                || (entry["demo"] == true) != (meta["demo"] == true)
            {
                return Err("Membri non corrispondenti al manifest".into());
            }
            let release = json!([
                meta["sql_release"],
                meta["history"]["history_release"],
                meta["quality"]["content_release"]
            ]);
            if let Some(expected) = &releases {
                if *expected != release {
                    return Err(
                        "Le lingue devono usare le stesse release SQL, History e XML".into(),
                    );
                }
            }
            releases = Some(release);
            let mut approved = entry.clone();
            approved["storage"] = json!(RULE);
            approved["source_sha256"] = json!(dumps::file_sha256(&path)?);
            tx.execute(
                "INSERT INTO datasets VALUES(?,?,?,?,?)",
                params![
                    language,
                    file,
                    encode(&approved)?,
                    encode(meta)?,
                    source.count
                ],
            )?;
            if let Some(path) = meta["dump_manifest"].as_str().map(Path::new) {
                for (kind, path, digest) in [
                    (
                        "sql_history",
                        path.to_owned(),
                        meta["dump_manifest_sha256"].clone(),
                    ),
                    (
                        "content",
                        path.parent()
                            .ok_or("Directory manifest mancante")?
                            .join(format!(
                                "content-{}.json",
                                meta["quality"]["content_release"].as_str().unwrap_or("")
                            )),
                        meta["quality"]["manifest_sha256"].clone(),
                    ),
                ] {
                    if path.is_file() {
                        if digest.as_str().is_some_and(|sha| {
                            dumps::file_sha256(&path).ok().as_deref() != Some(sha)
                        }) {
                            return Err("Manifest della fonte modificato".into());
                        }
                        tx.execute(
                            "INSERT INTO provenance VALUES(?,?,?)",
                            params![language, kind, encode(&read(&path)?)?],
                        )?;
                    } else if identity.get("sql_release").is_some() {
                        return Err("Manifest originale della fonte mancante".into());
                    }
                }
            }
            let mut query = source
                .database
                .prepare("SELECT id,data,position FROM pages ORDER BY position")?;
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? {
                crate::runtime::check_cancelled()?;
                let id: i64 = row.get(0)?;
                let data: String = row.get(1)?;
                let page: Value = serde_json::from_str(&data)?;
                let mut attributes = page.clone();
                for field in ["page_id", "title", "orphan"] {
                    attributes
                        .as_object_mut()
                        .ok_or("Pagina non valida")?
                        .remove(field);
                }
                let title = page["title"].as_str().ok_or("Titolo mancante")?;
                let category = page["category"].as_str();
                let (special, redirect) = hidden_category(category.unwrap_or(""))?;
                tx.execute(
                    "INSERT INTO pages VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    params![
                        language,
                        id,
                        row.get::<_, i64>(2)?,
                        title,
                        title.to_lowercase(),
                        page["orphan"].as_bool().ok_or("Stato orfana mancante")?,
                        category,
                        category.map(str::to_lowercase),
                        special,
                        redirect,
                        page["length_bytes"].as_i64(),
                        page["created_at"]
                            .as_str()
                            .map(chrono::DateTime::parse_from_rfc3339)
                            .transpose()?
                            .map(|time| time.timestamp_millis()),
                        page["topic_status"].as_str(),
                        encode(&attributes)?
                    ],
                )?;
                if page["topic_status"] == "predicted" {
                    if let Some(labels) = page["topic_labels"].as_array() {
                        for label in labels {
                            tx.execute(
                                "INSERT INTO topics VALUES(?,?,?)",
                                params![language, id, label.as_str().ok_or("Tema non valido")?],
                            )?;
                        }
                    }
                }
            }
            let mut query = source
                .database
                .prepare("SELECT id,data FROM candidates ORDER BY position")?;
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? {
                crate::runtime::check_cancelled()?;
                let id: i64 = row.get(0)?;
                let mut record: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
                let (shortlist, ids) = dumps::editorial_shortlist(&record, id, language)?;
                let shortlist = serde_json::to_value(shortlist)?;
                let candidates = record
                    .as_object_mut()
                    .ok_or("Candidate non valide")?
                    .remove("candidates")
                    .unwrap_or(json!([]));
                tx.execute(
                    "INSERT INTO targets VALUES(?,?,?,?,?)",
                    params![
                        language,
                        id,
                        record["candidate_count"].as_i64(),
                        record["status"].as_str(),
                        encode(&record)?
                    ],
                )?;
                let candidates = candidates.as_array().ok_or("Candidate non valide")?;
                if record["candidate_count"]
                    .as_u64()
                    .is_some_and(|n| n != candidates.len() as u64)
                {
                    return Err("Conteggio delle candidate non corrispondente".into());
                }
                for (position, candidate) in candidates.iter().enumerate() {
                    let mut candidate = candidate.clone();
                    let source = candidate["page_id"]
                        .as_i64()
                        .filter(|n| *n > 0 && *n != id)
                        .ok_or("ID candidata non valido")?;
                    let edges = candidate
                        .as_object_mut()
                        .ok_or("Candidata non valida")?
                        .remove("evidence")
                        .unwrap_or(json!([]));
                    let title = intern(
                        &tx,
                        candidate["title"]
                            .as_str()
                            .ok_or("Titolo candidata mancante")?,
                    )?;
                    for field in ["page_id", "title"] {
                        candidate
                            .as_object_mut()
                            .ok_or("Candidata non valida")?
                            .remove(field);
                    }
                    tx.execute(
                        "INSERT INTO candidates VALUES(?,?,?,?,?,?)",
                        params![language, id, source, position, title, encode(&candidate)?],
                    )?;
                    for (position, edge) in edges
                        .as_array()
                        .ok_or("Evidenze non valide")?
                        .iter()
                        .enumerate()
                    {
                        let reference = edge["language"]
                            .as_str()
                            .filter(|s| *s != language)
                            .ok_or("Lingua di riferimento non valida")?;
                        let source_title = intern(
                            &tx,
                            edge["source_title"]
                                .as_str()
                                .ok_or("Titolo riferimento mancante")?,
                        )?;
                        let target_title = intern(
                            &tx,
                            edge["target_title"]
                                .as_str()
                                .ok_or("Titolo riferimento mancante")?,
                        )?;
                        let mut attributes = edge.clone();
                        for field in ["source_title", "target_title", "language"] {
                            attributes
                                .as_object_mut()
                                .ok_or("Evidenza non valida")?
                                .remove(field);
                        }
                        tx.execute(
                            "INSERT INTO evidence VALUES(?,?,?,?,?,?,?,?)",
                            params![
                                language,
                                id,
                                source,
                                position,
                                reference,
                                source_title,
                                target_title,
                                encode(&attributes)?
                            ],
                        )?;
                    }
                }
                for (position, source) in ids.iter().enumerate() {
                    let selected = &shortlist[position]["evidence"][0];
                    let edges = candidates
                        .iter()
                        .find(|c| c["page_id"] == *source)
                        .and_then(|c| c["evidence"].as_array())
                        .ok_or("Evidenza shortlist mancante")?;
                    let edge = edges
                        .iter()
                        .position(|e| e == selected)
                        .ok_or("Evidenza shortlist non corrispondente")?;
                    tx.execute(
                        "INSERT INTO shortlists VALUES(?,?,?,?,?)",
                        params![language, id, source, position, edge],
                    )?;
                }
            }
            signatures.push((path, source.signature));
            manifest.push(approved);
        }
        if languages.is_empty() {
            return Err("Nessuna edizione completa".into());
        }
        if identity.get("sql_release").is_some() {
            if releases
                != Some(json!([
                    identity["sql_release"],
                    identity["history_release"],
                    identity["content_release"]
                ]))
            {
                return Err("Release pubblicate diverse da quelle richieste".into());
            }
            let expected = identity["languages"]
                .as_array()
                .ok_or("Lingue richieste mancanti")?
                .iter()
                .filter_map(|pair| pair[0].as_str().map(str::to_owned))
                .collect::<BTreeSet<_>>();
            if expected != languages {
                return Err("Manca una lingua della raccolta richiesta".into());
            }
        }
        for entry in entries.iter().filter(|e| e["kind"] == "aggregate") {
            if manifest.iter().any(|e| e["kind"] == "aggregate") {
                return Err("Una sola raccolta aggregata è consentita".into());
            }
            let index = read(&folder.join(filename(&entry["file"])?))?;
            let members = index["members"]
                .as_array()
                .ok_or("Membri aggregati mancanti")?;
            let codes = members
                .iter()
                .map(|m| m["language"].as_str().map(str::to_owned))
                .collect::<Option<BTreeSet<_>>>()
                .ok_or("Lingue aggregate non valide")?;
            if codes != languages
                || members.len() != languages.len()
                || index["metadata"]["complete"] != true
            {
                return Err("Aggregato incompleto".into());
            }
            for member in members {
                let (meta, count): (String, u64) = tx.query_row(
                    "SELECT metadata,page_count FROM datasets WHERE language=? AND file=?",
                    params![member["language"].as_str(), member["file"].as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if serde_json::from_str::<Value>(&meta)? != member["metadata"]
                    || member["page_count"] != count
                {
                    return Err("Aggregato non corrispondente ai membri".into());
                }
            }
            tx.execute(
                "INSERT INTO documents VALUES(?,?)",
                params![filename(&entry["file"])?, encode(&index)?],
            )?;
            let mut approved = entry.clone();
            approved["storage"] = json!(RULE);
            manifest.push(approved);
        }
        if languages.len() > 1 && !manifest.iter().any(|e| e["kind"] == "aggregate") {
            return Err("Aggregato completo richiesto".into());
        }
        for entry in &manifest {
            let (metadata, count) = if entry["kind"] == "aggregate" {
                let index: String = tx.query_row(
                    "SELECT data FROM documents WHERE path=?",
                    [filename(&entry["file"])?],
                    |r| r.get(0),
                )?;
                let index: Value = serde_json::from_str(&index)?;
                (
                    dumps::aggregate_summary_metadata(
                        index["members"].as_array().ok_or("Membri mancanti")?,
                        &index["metadata"],
                        true,
                    )?
                    .0,
                    tx.query_row("SELECT sum(page_count) FROM datasets", [], |r| {
                        r.get::<_, u64>(0)
                    })?,
                )
            } else {
                let (metadata, count): (String, u64) = tx.query_row(
                    "SELECT metadata,page_count FROM datasets WHERE language=?",
                    [entry["language"].as_str().ok_or("Lingua mancante")?],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                (serde_json::from_str::<Value>(&metadata)?, count)
            };
            for key in ["summary_file", "connections_metadata_file"] {
                if entry.get(key).is_some() {
                    let name = filename(&entry[key])?;
                    let value = read(&folder.join(name))?;
                    if value["metadata"] != metadata || value["census_page_count"] != count {
                        return Err("Riepilogo non corrispondente alla raccolta".into());
                    }
                    if key == "summary_file" {
                        if value["format"] != "dashboard_summary_v1"
                            || value["reports"]["overview"]["total"] != count
                        {
                            return Err("Riepilogo del censimento non valido".into());
                        }
                        let sources = value["sources"]
                            .as_array()
                            .ok_or("Fingerprint delle fonti mancanti")?;
                        let expected = if entry["kind"] == "aggregate" {
                            languages.len()
                        } else {
                            1
                        };
                        let mut seen = BTreeSet::new();
                        for source in sources {
                            let file = filename(&source["file"])?;
                            let language = source["language"]
                                .as_str()
                                .ok_or("Lingua della fonte mancante")?;
                            if !seen.insert(language) {
                                return Err("Fonte duplicata nel riepilogo".into());
                            }
                            let registered: String = tx.query_row(
                                "SELECT file FROM datasets WHERE language=?",
                                [language],
                                |r| r.get(0),
                            )?;
                            let (size, mtime) = dumps::file_signature(&folder.join(file))?;
                            if file != registered
                                || source["size"] != size
                                || source["mtime_ns"] != mtime
                            {
                                return Err("Fingerprint del riepilogo non corrispondente".into());
                            }
                        }
                        if sources.len() != expected {
                            return Err("Riepilogo di una raccolta incompleta".into());
                        }
                    }
                    tx.execute(
                        "INSERT INTO documents VALUES(?,?)",
                        params![name, encode(&value)?],
                    )?;
                }
            }
            if entry.get("creator_index_file").is_some() {
                let name = filename(&entry["creator_index_file"])?;
                visit_creators(&folder.join(name), &metadata, count, &mut |row| {
                    tx.execute(
                        "INSERT INTO creators VALUES(?,?,?,?,?,?,?)",
                        params![
                            filename(&entry["file"])?,
                            row["key"].as_str().ok_or("Creatore non valido")?,
                            row["key"]
                                .as_str()
                                .ok_or("Creatore non valido")?
                                .to_lowercase(),
                            row["articles"].as_i64(),
                            row["total"].as_i64().ok_or("Totale creatore mancante")?,
                            row["orphans"].as_i64().ok_or("Totale orfane mancante")?,
                            encode(&row)?
                        ],
                    )?;
                    Ok(())
                })?;
            }
        }
        let unknown: u64 = tx.query_row("SELECT COUNT(*) FROM evidence e WHERE NOT EXISTS(SELECT 1 FROM datasets d WHERE d.language=e.reference_language)",[],|r|r.get(0))?;
        if unknown != 0 {
            return Err("Evidenze di una lingua esterna alla raccolta".into());
        }
        for (path, signature) in signatures {
            if dumps::file_signature(&path)? != signature {
                return Err("Fonte modificata durante la pubblicazione".into());
            }
        }
        tx.execute(
            "INSERT INTO publication VALUES('identity',?)",
            [encode(identity)?],
        )?;
        tx.execute("INSERT INTO publication VALUES('rule',?)", [RULE])?;
        tx.execute(
            "INSERT INTO documents VALUES('manifest.json',?)",
            [encode(&json!(manifest))?],
        )?;
        tx.commit()?;
        let checksum = fingerprint(&db)?;
        db.execute(
            "INSERT INTO publication VALUES('integrity_sha256',?)",
            [checksum],
        )?;
        db.execute_batch("ANALYZE; PRAGMA optimize")?;
        let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err("Database non integro".into());
        }
        db.close().map_err(|(_, error)| error)?;
        File::open(&temporary)?.sync_all()?;
        std::fs::rename(&temporary, destination)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn visit_creators(
    path: &Path,
    metadata: &Value,
    count: u64,
    callback: &mut impl FnMut(Value) -> DumpResult<()>,
) -> DumpResult<()> {
    use serde::de::{DeserializeSeed, Error, IgnoredAny, MapAccess, SeqAccess, Visitor};
    struct Rows<'a, F>(&'a mut F);
    impl<'de, F: FnMut(Value) -> DumpResult<()>> Visitor<'de> for Rows<'_, F> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("creator rows")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            while let Some(row) = seq.next_element()? {
                (self.0)(row).map_err(A::Error::custom)?;
            }
            Ok(())
        }
    }
    impl<'de, F: FnMut(Value) -> DumpResult<()>> DeserializeSeed<'de> for Rows<'_, F> {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            d.deserialize_seq(self)
        }
    }
    struct Document<'a, F>(&'a mut F, &'a Value, u64);
    impl<'de, F: FnMut(Value) -> DumpResult<()>> Visitor<'de> for Document<'_, F> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("creator index")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            let mut found = false;
            let mut checked = BTreeSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if key == "creators" {
                    if found {
                        return Err(A::Error::custom("Duplicate creators"));
                    }
                    map.next_value_seed(Rows(&mut *self.0))?;
                    found = true;
                } else if matches!(key.as_str(), "format" | "metadata" | "census_page_count") {
                    if !checked.insert(key.clone()) {
                        return Err(A::Error::custom("Duplicate metadata"));
                    }
                    let value: Value = map.next_value()?;
                    let valid = match key.as_str() {
                        "format" => value == "creator_index_v1",
                        "metadata" => value == *self.1,
                        _ => value == self.2,
                    };
                    if !valid {
                        return Err(A::Error::custom("Stale creator index"));
                    }
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            if !found || checked.len() != 3 {
                return Err(A::Error::custom("Missing creators"));
            }
            Ok(())
        }
    }
    let mut decoder = serde_json::Deserializer::from_reader(BufReader::new(File::open(path)?));
    serde::de::Deserializer::deserialize_map(&mut decoder, Document(callback, metadata, count))?;
    decoder.end()?;
    Ok(())
}

fn hidden_category(category: &str) -> DumpResult<(bool, bool)> {
    static MARKS: std::sync::OnceLock<Result<regex::Regex, regex::Error>> =
        std::sync::OnceLock::new();
    let marks = MARKS
        .get_or_init(|| regex::Regex::new(r"\p{M}"))
        .as_ref()
        .map_err(|error| error.to_string())?;
    let normalized = category
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            '_' => ' ',
            c => c,
        })
        .collect::<String>();
    let normalized = marks
        .replace_all(&normalized, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let special = matches!(
        normalized.as_str(),
        "disambigua"
            | "disambigue"
            | "disambiguazione"
            | "disambiguazioni"
            | "disanbigua"
            | "disanbigue"
            | "registar da pagin da desambiguazziun"
            | "pagine di disambiguazione"
            | "disambiguation"
            | "disambiguation pages"
            | "pagines de desambiguacio"
            | "paginas de desambiguacao"
            | "homonymie"
            | "homonymies"
            | "insiemi di caratteri"
            | "set di caratteri"
            | "character sets"
            | "jeux de caracteres"
            | "conjunts de caracters"
            | "conjuntos de caracteres"
    );
    let redirect = matches!(
        normalized.as_str(),
        "redirect"
            | "redirects"
            | "pagine di redirect"
            | "redirezioni"
            | "redireccions"
            | "redirecionamentos"
            | "redirections"
    );
    Ok((special, redirect))
}

pub struct Census {
    db: Connection,
}

impl Census {
    /// Full bounded verification is for publication/reuse, never each HTTP query.
    pub fn verify(&self) -> DumpResult<()> {
        let expected: String = self.db.query_row(
            "SELECT data FROM publication WHERE key='integrity_sha256'",
            [],
            |r| r.get(0),
        )?;
        if fingerprint(&self.db)? != expected {
            return Err("Checksum della raccolta non corrispondente".into());
        }
        Ok(())
    }
    pub fn export_csv(&self, dataset: &str, writer: &mut impl std::io::Write) -> DumpResult<()> {
        let entry = self.entry(dataset)?;
        let language = entry["language"]
            .as_str()
            .filter(|l| *l != "all")
            .ok_or("Selezionare una singola edizione")?;
        writer.write_all(dumps::CSV_COLUMNS.join(",").as_bytes())?;
        writer.write_all(b"\r\n")?;
        let nonprintable = regex::Regex::new(r"[\p{C}\p{Z}]")?;
        let mut query=self.db.prepare("SELECT p.data,t.count,t.status,p.id,p.title,p.orphan FROM pages p LEFT JOIN targets t ON t.language=p.language AND t.id=p.id WHERE p.language=? ORDER BY p.position")?;
        let mut rows = query.query([language])?;
        while let Some(row) = rows.next()? {
            let page = decode_page(row.get(0)?, row.get(3)?, row.get(4)?, row.get(5)?)?;
            let count = json!(row.get::<_, Option<i64>>(1)?);
            let status = json!(row.get::<_, Option<String>>(2)?);
            dumps::write_csv_row(
                writer,
                dumps::CSV_COLUMNS.iter().map(|column| match *column {
                    "candidate_count" => &count,
                    "candidate_status" => &status,
                    _ => &page[column],
                }),
                &nonprintable,
            )?;
        }
        Ok(())
    }
    pub fn open(path: &Path) -> DumpResult<Self> {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.execute_batch("PRAGMA cache_size=-8192; PRAGMA temp_store=FILE")?;
        let rule: String =
            db.query_row("SELECT data FROM publication WHERE key='rule'", [], |r| {
                r.get(0)
            })?;
        if rule != RULE {
            return Err("Versione del database non supportata".into());
        }
        Ok(Self { db })
    }
    pub fn identity(&self) -> DumpResult<Value> {
        self.json("SELECT data FROM publication WHERE key='identity'", [])
    }
    fn json<P: rusqlite::Params>(&self, sql: &str, params: P) -> DumpResult<Value> {
        Ok(serde_json::from_str(&self.db.query_row(
            sql,
            params,
            |r| r.get::<_, String>(0),
        )?)?)
    }
    pub fn manifest(&self) -> DumpResult<Value> {
        self.json("SELECT data FROM documents WHERE path='manifest.json'", [])
    }
    fn entry(&self, dataset: &str) -> DumpResult<Value> {
        self.manifest()?
            .as_array()
            .and_then(|a| a.iter().find(|e| e["file"] == dataset))
            .cloned()
            .ok_or_else(|| "Raccolta non approvata".into())
    }
    fn meta(&self, language: &str) -> DumpResult<(Value, u64)> {
        let (meta, count): (String, u64) = self.db.query_row(
            "SELECT metadata,page_count FROM datasets WHERE language=?",
            [language],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((serde_json::from_str(&meta)?, count))
    }
    fn page(&self, language: &str, id: i64) -> DumpResult<Value> {
        let (data, title, orphan) = self.db.query_row(
            "SELECT data,title,orphan FROM pages WHERE language=? AND id=?",
            params![language, id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            },
        )?;
        decode_page(data, id, title, orphan)
    }
    fn record(&self, language: &str, id: i64, short: bool) -> DumpResult<Value> {
        let stored: Option<String> = self
            .db
            .query_row(
                "SELECT data FROM targets WHERE language=? AND id=?",
                params![language, id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(stored) = stored else {
            return Ok(json!({}));
        };
        let mut record: Value = serde_json::from_str(&stored)?;
        let sql = if short {
            "SELECT c.source,c.data,t.value,p.category FROM candidates c JOIN texts t ON t.id=c.title_id LEFT JOIN pages p ON p.language=c.language AND p.id=c.source JOIN shortlists s ON s.language=c.language AND s.target=c.target AND s.source=c.source WHERE c.language=? AND c.target=? ORDER BY s.position"
        } else {
            "SELECT c.source,c.data,t.value,p.category FROM candidates c JOIN texts t ON t.id=c.title_id LEFT JOIN pages p ON p.language=c.language AND p.id=c.source WHERE c.language=? AND c.target=? ORDER BY c.position"
        };
        let mut query = self.db.prepare(sql)?;
        let mut rows = query.query(params![language, id])?;
        let mut candidates = Vec::new();
        while let Some(row) = rows.next()? {
            let source: i64 = row.get(0)?;
            let mut candidate: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
            candidate["page_id"] = json!(source);
            candidate["title"] = json!(row.get::<_, String>(2)?);
            // Read existing census metadata for UI filters; never rewrite the collection.
            candidate["category"] = json!(row.get::<_, Option<String>>(3)?);
            let join = if short {
                " JOIN shortlists s ON s.language=e.language AND s.target=e.target AND s.source=e.source AND s.evidence_position=e.position"
            } else {
                ""
            };
            let mut edges=self.db.prepare(&format!("SELECT e.data,e.reference_language,a.value,b.value FROM evidence e JOIN texts a ON a.id=e.source_title_id JOIN texts b ON b.id=e.target_title_id{join} WHERE e.language=? AND e.target=? AND e.source=? ORDER BY e.position"))?;
            let mut rows = edges.query(params![language, id, source])?;
            let mut evidence = Vec::new();
            while let Some(row) = rows.next()? {
                let mut edge: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
                edge["language"] = json!(row.get::<_, String>(1)?);
                edge["source_title"] = json!(row.get::<_, String>(2)?);
                edge["target_title"] = json!(row.get::<_, String>(3)?);
                evidence.push(edge);
            }
            candidate["evidence"] = json!(evidence);
            candidates.push(candidate);
        }
        record["candidates"] = json!(candidates);
        Ok(record)
    }
    /// Resolve only registered virtual transports. The SQLite file itself is never public.
    pub fn resource(&self, path: &str) -> DumpResult<Value> {
        let direct: Option<String> = self
            .db
            .query_row("SELECT data FROM documents WHERE path=?", [path], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(value) = direct {
            return Ok(serde_json::from_str(&value)?);
        }
        for entry in self.manifest()?.as_array().ok_or("Manifest non valido")? {
            if entry["kind"] == "aggregate" {
                continue;
            }
            let language = entry["language"].as_str().ok_or("Lingua mancante")?;
            let (meta, count) = self.meta(language)?;
            for key in ["cards_dir", "connections_dir", "profile_topics_dir"] {
                let Some(folder) = entry[key].as_str() else {
                    continue;
                };
                let Some(leaf) = path.strip_prefix(&format!("{folder}/")) else {
                    continue;
                };
                if key == "profile_topics_dir" {
                    if leaf == "index.json" {
                        let mut query = self.db.prepare(
                            "SELECT DISTINCT id/1024 FROM pages WHERE language=? ORDER BY id/1024",
                        )?;
                        let shards = query
                            .query_map([language], |r| r.get::<_, i64>(0))?
                            .collect::<Result<Vec<_>, _>>()?;
                        return Ok(
                            json!({"format":"profile_topics_index_v1","metadata":meta,"census_page_count":count,"shard_size":1024,"shards":shards}),
                        );
                    }
                    let bucket = leaf
                        .strip_suffix(".json")
                        .ok_or("Shard non valido")?
                        .parse::<i64>()?;
                    if !(0..=9_007_199_254_740_991 / 1024).contains(&bucket) {
                        return Err("Shard non valido".into());
                    }
                    let mut query = self.db.prepare(
                        "SELECT data,id FROM pages WHERE language=? AND id>=? AND id<? ORDER BY id",
                    )?;
                    let mut rows =
                        query.query(params![language, bucket * 1024, (bucket + 1) * 1024])?;
                    let mut values = Vec::new();
                    while let Some(row) = rows.next()? {
                        let p: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
                        values.push(json!([
                            row.get::<_, i64>(1)?,
                            p["topic_status"],
                            p["topic_labels"]
                        ]));
                    }
                    return Ok(
                        json!({"format":"profile_topics_shard_v1","metadata":meta,"bucket":bucket,"rows":values}),
                    );
                }
                let id = leaf
                    .strip_suffix(".json")
                    .ok_or("Pagina non valida")?
                    .parse::<i64>()?;
                let page = self.page(language, id)?;
                if page["orphan"] != true {
                    return Err("Pagina non orfana".into());
                }
                let record = self.record(language, id, key == "cards_dir")?;
                if key == "cards_dir" {
                    let identity = dumps::canonical_json_sha256(&json!([RULE, meta]))?;
                    return Ok(
                        json!({"format":"orphan_card_v1","card_identity":identity,"census_page_count":count,"page":page,"candidate_record":record}),
                    );
                }
                let shared = self.resource(filename(&entry["connections_metadata_file"])?)?;
                return Ok(
                    json!({"format":"connections_v2","connection_identity":shared["connection_identity"],"census_page_count":count,"pages":[page],"link_candidates":{id.to_string():record}}),
                );
            }
        }
        Err("Risorsa non approvata".into())
    }

    pub fn query(&self, request: &PageQuery, closed: &[(String, u64, u64)]) -> DumpResult<Value> {
        request.validate()?;
        let entry = self.entry(&request.dataset)?;
        let aggregate = entry["kind"] == "aggregate";
        let language = entry["language"].as_str().ok_or("Lingua mancante")?;
        let summary = self.resource(filename(&entry["summary_file"])?)?;
        self.db.execute_batch("CREATE TEMP TABLE IF NOT EXISTS closed(language TEXT,target INTEGER,source INTEGER,PRIMARY KEY(language,target,source)) WITHOUT ROWID;
            CREATE TEMP TABLE IF NOT EXISTS weights(label TEXT PRIMARY KEY,weight REAL) WITHOUT ROWID;
            CREATE TEMP TABLE IF NOT EXISTS affinities(language TEXT PRIMARY KEY,weight REAL) WITHOUT ROWID;
            DELETE FROM closed; DELETE FROM weights; DELETE FROM affinities;")?;
        {
            let tx = self.db.unchecked_transaction()?;
            for (language, target, source) in closed {
                tx.execute(
                    "INSERT OR IGNORE INTO closed VALUES(?,?,?)",
                    params![language, target, source],
                )?;
            }
            // Reuse the scientific Rust scoring function on singleton labels. The
            // SQL sum is exactly the same additive score, without loading a census.
            if !request.edits.is_empty() {
                let labels: Vec<String> =
                    serde_json::from_str(&crate::dashboard::calculate("taxonomy", "null")?)?;
                let candidates = labels
                    .iter()
                    .map(|label| json!({"key":label,"language":"","topic_labels":[label]}))
                    .collect::<Vec<_>>();
                let input =
                    json!({"edits":request.edits,"candidates":candidates,"aggregate":false});
                let report: Value = serde_json::from_str(&crate::dashboard::calculate(
                    "interests",
                    &encode(&input)?,
                )?)?;
                for score in report["scores"].as_array().ok_or("Profilo non valido")? {
                    tx.execute(
                        "INSERT INTO weights VALUES(?,?)",
                        params![score["key"].as_str(), score["topic"].as_f64()],
                    )?;
                }
                let langs = request
                    .edits
                    .iter()
                    .filter_map(|e| e["language"].as_str())
                    .collect::<BTreeSet<_>>();
                for lang in langs {
                    let count = request
                        .edits
                        .iter()
                        .filter(|e| e["language"] == lang)
                        .count();
                    tx.execute(
                        "INSERT INTO affinities VALUES(?,?)",
                        params![lang, count as f64 / request.edits.len() as f64],
                    )?;
                }
            }
            tx.commit()?;
        }
        if request.kind == "creators" {
            return self.creators(request, &summary);
        }
        let mut values = Vec::<rusqlite::types::Value>::new();
        let mut conditions = Vec::new();
        if !aggregate {
            conditions.push("p.language=?".to_owned());
            values.push(language.to_owned().into());
        }
        if request.kind == "cards" || request.group == "orphans" {
            conditions.push("p.orphan=1".into());
        } else if request.group == "non-orphans" {
            conditions.push("p.orphan=0".into());
        }
        if request.kind == "cards" {
            if !request.show_disambiguation {
                conditions.push("p.special=0".into());
            }
            if !request.show_redirects {
                conditions.push("p.redirect=0".into());
            }
            if !request.show_without_connections {
                conditions.push("t.count>0".into());
            }
            if !request.hidden_categories.is_empty() {
                let placeholders = vec!["?"; request.hidden_categories.len()].join(",");
                conditions.push(format!(
                    "coalesce(replace(trim(p.category_fold),'_',' '),'') NOT IN ({placeholders})"
                ));
                values.extend(request.hidden_categories.iter().cloned().map(Into::into));
            }
        }
        if !request.search.is_empty() {
            conditions.push("instr(p.title_fold,?)>0".into());
            values.push(request.search.to_lowercase().into());
        }
        // Classification labels preserve empty and unavailable predictions, and
        // use the browser's translated missing labels without persisting them.
        let label = if request.classification == "categories" {
            let prefix = if aggregate {
                "upper(p.language)||' · '||"
            } else {
                ""
            };
            format!("{prefix}coalesce(p.category,?)")
        } else {
            let topic = if request.classification == "macro" {
                "substr(label,1,instr(label,'.')-1)"
            } else {
                "substr(label,1,instr(label,'.')-1)||' · '||substr(label,instr(label,'.')+1)"
            };
            format!("CASE WHEN p.topic_status IS NULL OR p.topic_status!='predicted' THEN ? WHEN NOT EXISTS(SELECT 1 FROM topics x WHERE x.language=p.language AND x.page_id=p.id) THEN ? ELSE (SELECT group_concat(label,' · ') FROM (SELECT DISTINCT {topic} AS label FROM topics x WHERE x.language=p.language AND x.page_id=p.id ORDER BY label)) END")
        };
        let labels = if request.classification == "categories" {
            vec![request.missing_category.clone()]
        } else {
            vec![request.missing_topics.clone(), request.empty_topics.clone()]
        };
        // A CTE binds translated labels once, shared by filter and ordering.
        let prefix=format!("WITH listed AS (SELECT p.*,t.count AS candidate_count,{label} AS display_label,
            EXISTS(SELECT 1 FROM shortlists s WHERE s.language=p.language AND s.target=p.id) AND NOT EXISTS(SELECT 1 FROM shortlists s WHERE s.language=p.language AND s.target=p.id AND NOT EXISTS(SELECT 1 FROM closed c WHERE c.language=s.language AND c.target=s.target AND c.source=s.source)) AS reviewed,
            coalesce((SELECT sum(w.weight) FROM topics x JOIN weights w ON w.label=x.label WHERE x.language=p.language AND x.page_id=p.id),0) AS topic_score,
            coalesce((SELECT weight FROM affinities a WHERE a.language=p.language),0) AS language_score
            FROM pages p LEFT JOIN targets t ON t.language=p.language AND t.id=p.id{} )",if conditions.is_empty(){String::new()}else{format!(" WHERE {}",conditions.join(" AND "))});
        let mut bindings = labels
            .into_iter()
            .map(rusqlite::types::Value::from)
            .collect::<Vec<_>>();
        bindings.extend(values);
        let mut filter = String::new();
        if !request.category.is_empty() {
            if request.classification == "categories" {
                bindings.push(request.missing_category.to_lowercase().into());
                filter = if aggregate {
                    " WHERE instr(lower(language)||' · '||coalesce(category_fold,?),?)>0"
                } else {
                    " WHERE instr(coalesce(category_fold,?),?)>0"
                }
                .into();
            } else {
                let topic = if request.classification == "macro" {
                    "substr(x.label,1,instr(x.label,'.')-1)"
                } else {
                    "substr(x.label,1,instr(x.label,'.')-1)||' · '||substr(x.label,instr(x.label,'.')+1)"
                };
                let contains=" WHERE ((topic_status IS NULL OR topic_status!='predicted') AND instr(?,?)>0 OR topic_status='predicted' AND NOT EXISTS(SELECT 1 FROM topics x WHERE x.language=listed.language AND x.page_id=listed.id) AND instr(?,?)>0 OR EXISTS(SELECT 1 FROM topics x WHERE x.language=listed.language AND x.page_id=listed.id AND instr(lower(";
                filter = format!("{contains}{topic}),?)>0))");
                bindings.push(request.missing_topics.to_lowercase().into());
                bindings.push(request.category.to_lowercase().into());
                bindings.push(request.empty_topics.to_lowercase().into());
                bindings.push(request.category.to_lowercase().into());
            }
            bindings.push(request.category.to_lowercase().into());
        }
        let total: u64 = self.db.query_row(
            &format!("{prefix} SELECT count(*) FROM listed{filter}"),
            rusqlite::params_from_iter(&bindings),
            |r| r.get(0),
        )?;
        let direction = if request.order.ends_with("asc") {
            "ASC"
        } else {
            "DESC"
        };
        let field = if request.order.starts_with("length") {
            Some("length")
        } else if request.order.starts_with("date") {
            Some("created")
        } else if request.order.starts_with("candidates") {
            Some("candidate_count")
        } else {
            None
        };
        let mut order = if let Some(field) = field {
            format!("{field} IS NULL,{field} {direction},")
        } else if request.order == "category-asc" {
            "display_label,".into()
        } else {
            String::new()
        };
        if request.order == "personalized" {
            order = format!(
                "topic_score DESC,{}coalesce(candidate_count,0) DESC,",
                if aggregate {
                    "language_score DESC,"
                } else {
                    ""
                }
            );
        }
        if request.kind == "cards" {
            order = format!("reviewed,{order}");
        }
        bindings.push(PAGE_SIZE.into());
        bindings.push(request.offset.into());
        let mut query=self.db.prepare(&format!("{prefix} SELECT language,id,data,title,orphan FROM listed{filter} ORDER BY {order}title_fold,title,id,language LIMIT ? OFFSET ?"))?;
        let mut rows = query.query(rusqlite::params_from_iter(bindings))?;
        let mut pages = Vec::with_capacity(PAGE_SIZE as usize);
        let mut records = serde_json::Map::new();
        let mut transports = serde_json::Map::new();
        while let Some(row) = rows.next()? {
            let language: String = row.get(0)?;
            let id: i64 = row.get(1)?;
            let mut page = decode_page(row.get(2)?, id, row.get(3)?, row.get(4)?)?;
            page["language"] = json!(language);
            let record = if request.kind == "cards" {
                let mut record = self.record(&language, id, true)?;
                let ids = record["candidates"]
                    .as_array()
                    .map(|a| a.iter().map(|c| c["page_id"].clone()).collect::<Vec<_>>())
                    .unwrap_or_default();
                record
                    .as_object_mut()
                    .ok_or("Record non valido")?
                    .remove("candidates");
                record["candidate_ids"] = json!(ids);
                record
            } else {
                self.db.query_row("SELECT count,status FROM targets WHERE language=? AND id=?",params![language,id],|r|Ok(json!({"candidate_count":r.get::<_,Option<i64>>(0)?,"status":r.get::<_,Option<String>>(1)?}))).optional()?.unwrap_or(json!({}))
            };
            records.insert(
                if aggregate {
                    format!("{language}:{id}")
                } else {
                    id.to_string()
                },
                record,
            );
            if request.kind == "cards" && !transports.contains_key(&language) {
                let (metadata, count) = self.meta(&language)?;
                let entry =
                    self.json("SELECT entry FROM datasets WHERE language=?", [&language])?;
                transports.insert(language,json!({"directory":entry["cards_dir"],"identity":dumps::canonical_json_sha256(&json!([RULE,metadata]))?,"metadata":metadata,"census_page_count":count}));
            }
            pages.push(page);
        }
        Ok(
            json!({"format":"page_results_v1","dataset":request.dataset,"metadata":summary["metadata"],"census_page_count":summary["census_page_count"],"offset":request.offset,"total":total,"pages":pages,"link_candidates":records,"card_transports":transports}),
        )
    }
    fn creators(&self, request: &PageQuery, summary: &Value) -> DumpResult<Value> {
        let field = if request.order.starts_with("articles") {
            if !matches!(
                summary["metadata"]["creators"]["status"].as_str(),
                Some("collected" | "partially_collected")
            ) && matches!(
                summary["metadata"]["history"]["status"].as_str(),
                Some("collected" | "partially_collected")
            ) {
                "total"
            } else {
                "articles"
            }
        } else {
            "orphans"
        };
        let direction = if request.order.ends_with("asc") {
            "ASC"
        } else {
            "DESC"
        };
        let total: u64 = self.db.query_row(
            "SELECT count(*) FROM creators WHERE dataset=? AND instr(key_fold,?)>0",
            params![request.dataset, request.search.to_lowercase()],
            |r| r.get(0),
        )?;
        let mut query=self.db.prepare(&format!("SELECT data FROM creators WHERE dataset=? AND instr(key_fold,?)>0 ORDER BY {field} IS NULL,{field} {direction},key LIMIT {PAGE_SIZE} OFFSET ?"))?;
        let rows = query
            .query_map(
                params![
                    request.dataset,
                    request.search.to_lowercase(),
                    request.offset
                ],
                |r| r.get::<_, String>(0),
            )?
            .map(|r| Ok(serde_json::from_str::<Value>(&r?)?))
            .collect::<DumpResult<Vec<_>>>()?;
        Ok(
            json!({"format":"creator_results_v1","dataset":request.dataset,"offset":request.offset,"total":total,"creators":rows}),
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub dataset: String,
    pub kind: String,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub category: String,
    #[serde(default = "default_group")]
    pub group: String,
    #[serde(default = "default_classification")]
    pub classification: String,
    #[serde(default = "default_order")]
    pub order: String,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub show_disambiguation: bool,
    #[serde(default)]
    pub show_redirects: bool,
    #[serde(default)]
    pub show_without_connections: bool,
    #[serde(default)]
    pub hidden_categories: Vec<String>,
    #[serde(default)]
    pub edits: Vec<Value>,
    #[serde(default)]
    pub closed_pairs: Vec<(String, u64, u64)>,
    #[serde(default)]
    pub missing_category: String,
    #[serde(default)]
    pub missing_topics: String,
    #[serde(default)]
    pub empty_topics: String,
}
fn default_group() -> String {
    "orphans".into()
}
fn default_classification() -> String {
    "categories".into()
}
fn default_order() -> String {
    "title-asc".into()
}
impl PageQuery {
    fn validate(&self) -> DumpResult<()> {
        if !matches!(self.kind.as_str(), "articles" | "cards" | "creators")
            || !matches!(
                self.classification.as_str(),
                "categories" | "macro" | "topics"
            )
            || !matches!(self.group.as_str(), "all" | "orphans" | "non-orphans")
            || !matches!(
                self.order.as_str(),
                "title-asc"
                    | "category-asc"
                    | "length-asc"
                    | "length-desc"
                    | "date-asc"
                    | "date-desc"
                    | "candidates-asc"
                    | "candidates-desc"
                    | "orphans-asc"
                    | "orphans-desc"
                    | "articles-asc"
                    | "articles-desc"
                    | "personalized"
            )
            || self.offset < 0
            || self.offset % PAGE_SIZE != 0
            || self.edits.len() > 100
            || self.closed_pairs.len() > 100000
            || self.hidden_categories.len() > 128
            || self
                .hidden_categories
                .iter()
                .any(|category| category.len() > 256)
            || self.closed_pairs.iter().any(|(language, target, source)| {
                crate::wiki_database(language, None).is_err()
                    || *target == 0
                    || *source == 0
                    || *target > 9_007_199_254_740_991
                    || *source > 9_007_199_254_740_991
            })
            || [
                &self.search,
                &self.category,
                &self.missing_category,
                &self.missing_topics,
                &self.empty_topics,
            ]
            .iter()
            .any(|s| s.len() > 2048)
        {
            return Err("Parametri di selezione non validi".into());
        }
        Ok(())
    }
}

use super::*;
use crate::dashboard::RegistrationCohortCounts;
use std::path::PathBuf;

fn publication_lock(destination: &Path) -> DumpResult<File> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(destination.with_extension("publish.lock"))?;
    lock.try_lock()
        .map_err(|_| "Un'altra pubblicazione sta usando il database")?;
    Ok(lock)
}

fn verify_and_close(db: Connection) -> DumpResult<()> {
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err("Database non integro".into());
    }
    db.close().map_err(|(_, error)| error)?;
    Ok(())
}

fn install_collection(temporary: &Path, destination: &Path, parent: &Path) -> DumpResult<()> {
    File::open(temporary)?.sync_all()?;
    crate::runtime::check_cancelled()?;
    std::fs::rename(temporary, destination)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Atomically replace the publication only after every member and all evidence
/// have been imported and verified. Neither source exports nor the old DB change
/// on failure. Only a fresh, privately owned temporary DB can be removed here.
pub fn publish(folder: &Path, destination: &Path, identity: &Value) -> DumpResult<()> {
    if destination.extension().and_then(|s| s.to_str()) != Some("sqlite") {
        return Err("Il database deve avere estensione .sqlite".into());
    }
    let selected = select_entries(folder)?;
    if selected
        .iter()
        .any(|entry| entry["storage"] == dumps::COLLECTION_STAGING)
        && !dumps::collection_staging(folder)?
    {
        return Err("Area privata della raccolta mancante".into());
    }
    let entries = &selected;
    let parent = destination
        .parent()
        .ok_or("Directory del database mancante")?;
    std::fs::create_dir_all(parent)?;
    let _lock = publication_lock(destination)?;
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
            CREATE INDEX page_global_titles ON pages(title_fold,title,id,language);
            CREATE INDEX page_global_groups ON pages(orphan,title_fold,title,id,language);
            CREATE INDEX topic_labels ON topics(label,language,page_id);
            CREATE INDEX target_counts ON targets(language,count,id);
            CREATE INDEX creator_orphans ON creators(dataset,orphans,key);
            CREATE INDEX creator_articles ON creators(dataset,articles,key);
            CREATE INDEX creator_totals ON creators(dataset,total,key);")?;
        let tx = db.transaction()?;
        tx.execute_batch("CREATE TEMP TABLE support(language TEXT,target INTEGER,source INTEGER,editions INTEGER,first_edge INTEGER,PRIMARY KEY(language,source,target)) WITHOUT ROWID")?;
        let mut manifest = Vec::new();
        let mut languages = BTreeSet::new();
        let mut releases = None;
        let mut signatures = Vec::new();
        for entry in entries.iter().filter(|e| e["kind"] != "aggregate") {
            manifest.push(import_member(
                &tx,
                folder,
                entry,
                &mut languages,
                &mut releases,
                &mut signatures,
            )?);
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
                if key == "summary_file" || entry["kind"] != "aggregate" {
                    let name = filename(&entry[key])?;
                    let value = read(&folder.join(name))?;
                    if value["metadata"] != metadata || value["census_page_count"] != count {
                        return Err(format!(
                            "Risorsa {name}: riepilogo non corrispondente alla raccolta"
                        )
                        .into());
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
            {
                let name = filename(&entry["creator_index_file"])?;
                visit_creators(&folder.join(name), &metadata, count, &mut |row| {
                    tx.prepare_cached("INSERT INTO creators VALUES(?,?,?,?,?,?,?)")?
                        .execute(params![
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
                        ])?;
                    Ok(())
                })?;
            }
        }
        let missing: u64 = tx.query_row("SELECT count(*) FROM pages p WHERE p.orphan=1 AND NOT EXISTS(SELECT 1 FROM targets t WHERE t.language=p.language AND t.id=p.id)", [], |r| r.get(0))?;
        if missing != 0 {
            return Err("Risultati del confronto mancanti per pagine orfane".into());
        }
        let unknown: u64 = tx.query_row("SELECT COUNT(*) FROM evidence e WHERE NOT EXISTS(SELECT 1 FROM datasets d WHERE d.language=e.reference_language)",[],|r|r.get(0))?;
        if unknown != 0 {
            return Err("Evidenze di una lingua esterna alla raccolta".into());
        }
        if languages.len() > 1 {
            for language in &languages {
                annotate_local_mentions(
                    &tx,
                    language,
                    identity["editorial_ranking"] == EDITORIAL_RANKING,
                )?;
            }
        }
        rank_shortlists(&tx)?;
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
        verify_and_close(db)?;
        install_collection(&temporary, destination, parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn registration_summary_updates(census: &Census) -> DumpResult<Vec<(String, String)>> {
    let manifest = census.manifest()?;
    let entries = manifest.as_array().ok_or("Manifest non valido")?;
    let mut pages = census.db.prepare(
        "SELECT orphan,
           CASE WHEN json_type(data,'$.creator_registered_at')='text' THEN json_extract(data,'$.creator_registered_at') END,
           CASE WHEN json_type(data,'$.page_created_at')='text' THEN json_extract(data,'$.page_created_at') END
         FROM pages WHERE language=?",
    )?;
    let mut aggregate = RegistrationCohortCounts::default();
    let mut updates = Vec::new();
    let mut check_summary = |entry: &Value, counts: &RegistrationCohortCounts| -> DumpResult<()> {
        let name = filename(&entry["summary_file"])?;
        let summary = census.resource(name)?;
        let [orphans, others] = counts.group_totals();
        if summary["format"] != "dashboard_summary_v1"
            || summary["metadata"]["language"] != entry["language"]
            || summary["metadata"]["finished_at"] != entry["date"]
            || summary["census_page_count"] != orphans + others
            || summary["reports"]["overview"]["total"] != orphans + others
            || summary["reports"]["overview"]["orphans"] != orphans
        {
            return Err(format!("Riepilogo {name} non corrispondente al censimento").into());
        }
        let report = serde_json::to_value(counts.report())?;
        if summary["reports"]["registration_cohorts"] != report {
            updates.push((name.to_owned(), encode(&report)?));
        }
        Ok(())
    };
    for entry in entries.iter().filter(|entry| entry["kind"] != "aggregate") {
        let language = entry["language"].as_str().ok_or("Lingua mancante")?;
        let mut member = RegistrationCohortCounts::default();
        let mut rows = pages.query([language])?;
        while let Some(row) = rows.next()? {
            crate::runtime::check_cancelled()?;
            let registered: Option<String> = row.get(1)?;
            let created: Option<String> = row.get(2)?;
            member.add_dates(row.get(0)?, registered.as_deref(), created.as_deref());
        }
        check_summary(entry, &member)?;
        aggregate.merge(&member);
    }
    for entry in entries.iter().filter(|entry| entry["kind"] == "aggregate") {
        check_summary(entry, &aggregate)?;
    }
    Ok(updates)
}

/// Prepare the missing or changed chart reports from an approved local census.
/// The old database remains accessible, including to readers with open handles.
pub fn refresh_registration_cohorts(destination: &Path) -> DumpResult<Option<PathBuf>> {
    let destination = std::fs::canonicalize(destination)?;
    let parent = destination
        .parent()
        .ok_or("Directory del database mancante")?;
    let _lock = publication_lock(&destination)?;
    let census = Census::open(&destination)?;
    census.verify_metadata()?;
    census.verify()?;
    let updates = registration_summary_updates(&census)?;
    if updates.is_empty() {
        return Ok(None);
    }

    let generation = crate::runtime::unique_id();
    let temporary = parent.join(format!(".collection-{generation}.sqlite"));
    File::create_new(&temporary)?;
    let result = (|| -> DumpResult<PathBuf> {
        let mut db = Connection::open(&temporary)?;
        rusqlite::backup::Backup::new(&census.db, &mut db)?.run_to_completion(
            4096,
            std::time::Duration::from_millis(1),
            None,
        )?;
        let tx = db.transaction()?;
        for (name, report) in updates {
            if tx.execute(
                "UPDATE documents SET data=json_set(data,'$.reports.registration_cohorts',json(?)) WHERE path=?",
                params![report, name],
            )? != 1 {
                return Err(format!("Riepilogo {name} mancante").into());
            }
        }
        let checksum = fingerprint(&tx)?;
        if tx.execute(
            "UPDATE publication SET data=? WHERE key='integrity_sha256'",
            [checksum],
        )? != 1
        {
            return Err("Impronta della raccolta mancante".into());
        }
        tx.commit()?;
        verify_and_close(db)?;
        crate::runtime::check_cancelled()?;
        let backup =
            destination.with_extension(format!("before-registration-cohorts-{generation}.sqlite"));
        std::fs::hard_link(&destination, &backup)?;
        File::open(parent)?.sync_all()?;
        install_collection(&temporary, &destination, parent)?;
        Ok(backup)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map(Some)
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

fn select_entries(folder: &Path) -> DumpResult<Vec<Value>> {
    let manifest = read(&folder.join("manifest.json"))?;
    let entries = manifest
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("Manifest mancante")?;
    let rank = |entry: &Value| -> DumpResult<(String, Option<String>, String)> {
        let date = entry["date"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Data della raccolta mancante")?;
        let published = entry
            .get("published_at")
            .map(|v| v.as_str().ok_or("Data di pubblicazione non valida"))
            .transpose()?;
        Ok((
            date.to_owned(),
            published.map(str::to_owned),
            filename(&entry["file"])?.to_owned(),
        ))
    };
    let mut latest = std::collections::BTreeMap::<String, Value>::new();
    for entry in entries.iter().filter(|e| e["kind"] != "aggregate") {
        let language = entry["language"].as_str().ok_or("Lingua mancante")?;
        crate::wiki_database(language, None)?;
        let current = rank(entry)?;
        if !latest.contains_key(language) || current > rank(&latest[language])? {
            latest.insert(language.to_owned(), entry.clone());
        }
    }
    let mut selected = latest.into_values().collect::<Vec<_>>();
    if selected.is_empty() {
        return Err("Nessuna edizione completa".into());
    }
    if selected.len() > 1 {
        let mut matched = None;
        for entry in entries.iter().filter(|e| e["kind"] == "aggregate").rev() {
            let index = read(&folder.join(filename(&entry["file"])?))?;
            let members = index["members"]
                .as_array()
                .ok_or("Membri aggregati mancanti")?;
            if members.len() == selected.len()
                && selected.iter().all(|e| {
                    members
                        .iter()
                        .any(|m| m["file"] == e["file"] && m["language"] == e["language"])
                })
            {
                matched = Some(entry.clone());
                break;
            }
        }
        selected.push(matched.ok_or("Aggregato della raccolta recente mancante")?);
    }
    Ok(selected)
}

fn import_member(
    tx: &rusqlite::Transaction<'_>,
    folder: &Path,
    entry: &Value,
    languages: &mut BTreeSet<String>,
    releases: &mut Option<Value>,
    signatures: &mut Vec<(std::path::PathBuf, (u64, i64))>,
) -> DumpResult<Value> {
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
        meta["sql_release"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Release SQL mancante")?,
        meta["history"]["history_release"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Release History mancante")?,
        meta["quality"]["content_release"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Release XML mancante")?
    ]);
    if let Some(expected) = &*releases {
        if *expected != release {
            return Err("Le lingue devono usare le stesse release SQL, History e XML".into());
        }
    }
    *releases = Some(release);
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
                        meta["quality"]["content_release"]
                            .as_str()
                            .ok_or("Release XML mancante")?
                    )),
                meta["quality"]["manifest_sha256"].clone(),
            ),
        ] {
            if path.is_file() {
                let sha = digest.as_str().ok_or("Checksum manifest mancante")?;
                if dumps::file_sha256(&path)? != sha {
                    return Err("Manifest della fonte modificato".into());
                }
                tx.execute(
                    "INSERT INTO provenance VALUES(?,?,?)",
                    params![language, kind, encode(&read(&path)?)?],
                )?;
            } else {
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
        let (special, redirect) = category
            .map(hidden_category)
            .transpose()?
            .unwrap_or((false, false));
        tx.prepare_cached("INSERT INTO pages VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)")?
            .execute(params![
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
            ])?;
        if page["topic_status"] == "predicted" {
            if let Some(labels) = page["topic_labels"].as_array() {
                for label in labels {
                    tx.prepare_cached("INSERT INTO topics VALUES(?,?,?)")?
                        .execute(params![
                            language,
                            id,
                            label.as_str().ok_or("Tema non valido")?
                        ])?;
                }
            }
        }
    }
    let mut title_cache = std::collections::HashMap::new();
    let mut query = source
        .database
        .prepare("SELECT id,data FROM candidates ORDER BY position")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        crate::runtime::check_cancelled()?;
        let id: i64 = row.get(0)?;
        let mut record: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
        let candidates = record
            .as_object_mut()
            .ok_or("Candidate non valide")?
            .remove("candidates")
            .ok_or("Candidate mancanti")?;
        tx.prepare_cached("INSERT INTO targets VALUES(?,?,?,?,?)")?
            .execute(params![
                language,
                id,
                record["candidate_count"].as_i64(),
                record["status"].as_str(),
                encode(&record)?
            ])?;
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
                .ok_or("Evidenze mancanti")?;
            let edges = edges.as_array().ok_or("Evidenze non valide")?;
            let mut references = BTreeSet::new();
            let mut first_edge = None;
            for (index, edge) in edges
                .iter()
                .enumerate()
                .filter(|(_, edge)| dumps::editorial_reference(edge, language))
            {
                references.insert(edge["language"].as_str().ok_or("Lingua mancante")?);
                first_edge.get_or_insert(index);
            }
            candidate["reference_count"] = json!(references.len());
            // These are properties of this publication's XML, never an inherited assertion.
            let fields = candidate.as_object_mut().ok_or("Candidata non valida")?;
            fields.remove("local_mention");
            fields.remove("mention_revision");
            let first_edge = first_edge.filter(|_| dumps::editorial_title(&candidate["title"]));
            tx.prepare_cached("INSERT INTO support VALUES(?,?,?,?,?)")?
                .execute(params![language, id, source, references.len(), first_edge])?;
            let title = intern(
                tx,
                &mut title_cache,
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
            tx.prepare_cached("INSERT INTO candidates VALUES(?,?,?,?,?,?)")?
                .execute(params![
                    language,
                    id,
                    source,
                    position,
                    title,
                    encode(&candidate)?
                ])?;
            for (position, edge) in edges.iter().enumerate() {
                let reference = edge["language"]
                    .as_str()
                    .filter(|s| *s != language)
                    .ok_or("Lingua di riferimento non valida")?;
                let source_title = intern(
                    tx,
                    &mut title_cache,
                    edge["source_title"]
                        .as_str()
                        .ok_or("Titolo riferimento mancante")?,
                )?;
                let target_title = intern(
                    tx,
                    &mut title_cache,
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
                tx.prepare_cached("INSERT INTO evidence VALUES(?,?,?,?,?,?,?,?)")?
                    .execute(params![
                        language,
                        id,
                        source,
                        position,
                        reference,
                        source_title,
                        target_title,
                        encode(&attributes)?
                    ])?;
            }
        }
    }
    signatures.push((path, source.signature));
    Ok(approved)
}

fn rank_shortlists(db: &Connection) -> DumpResult<()> {
    // Keep one valid reference in each compact card, but rank against all
    // distinct supporting editions before applying the existing per-group cap.
    db.execute_batch("INSERT INTO shortlists(language,target,source,position,evidence_position)
        SELECT language,target,source,position,first_edge FROM (
          SELECT c.language,c.target,c.source,s.first_edge,
            row_number() OVER (PARTITION BY c.language,c.target ORDER BY
              json_extract(c.data,'$.local_mention') IS 1 DESC,s.editions DESC,
              json_extract(c.data,'$.dead_end') IS 1 DESC,c.position) AS position,
            row_number() OVER (PARTITION BY c.language,c.target,json_extract(c.data,'$.dead_end') IS 1 ORDER BY
              json_extract(c.data,'$.local_mention') IS 1 DESC,s.editions DESC,c.position) AS group_position
          FROM candidates c JOIN support s USING(language,target,source) WHERE s.first_edge IS NOT NULL
        ) WHERE group_position<=5;
        DROP TABLE support;")?;
    Ok(())
}

fn annotate_local_mentions(db: &Connection, language: &str, required: bool) -> DumpResult<()> {
    if !db.query_row(
        "SELECT EXISTS(SELECT 1 FROM candidates WHERE language=?)",
        [language],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    let input:Option<(String,String)>=db.query_row(
        "SELECT p.data,d.metadata FROM provenance p JOIN datasets d USING(language) WHERE p.language=? AND p.kind='content'",
        [language],|r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((manifest, metadata)) = input else {
        if required {
            return Err("Manifest XML mancante per le menzioni locali".into());
        }
        return Ok(());
    };
    let metadata: Value = serde_json::from_str(&metadata)?;
    let mut targets = db.prepare("SELECT c.target,p.title FROM support c JOIN pages p ON p.language=c.language AND p.id=c.target WHERE c.language=? AND c.source=? ORDER BY c.target")?;
    let mut update = db.prepare("UPDATE candidates SET data=json_set(data,'$.local_mention',json(?),'$.mention_revision',?) WHERE language=? AND source=? AND target=?")?;
    let syntax = regex::Regex::new(
        r#"(?is)<!--.*?(?:-->|$)|https?://[^\s<>{}\[\]]+|\{\{|\}\}|\[\[|\]\]|\[|\]|</?[a-z](?:[^>'"]|"[^"]*"|'[^']*')*>|\{\||\|\}"#,
    )?;
    dumps::scan_editorial_content(
        &serde_json::from_str(&manifest)?,
        language,
        &crate::wiki_database(language, metadata["database"].as_str())?,
        metadata["quality"]["content_release"]
            .as_str()
            .ok_or("Release XML mancante")?,
        required,
        |source, revision, text| {
            let mut rows = targets.query(params![language, source])?;
            let Some(mut row) = rows.next()? else {
                return Ok(());
            };
            let body = editorial_body(text, &syntax);
            loop {
                let target: i64 = row.get(0)?;
                let title: String = row.get(1)?;
                let title = title.replace('_', " ").to_lowercase();
                let mentioned = !title.is_empty()
                    && body.match_indices(&title).any(|(i, _)| {
                        !body[..i]
                            .chars()
                            .next_back()
                            .is_some_and(char::is_alphanumeric)
                            && !body[i + title.len()..]
                                .chars()
                                .next()
                                .is_some_and(char::is_alphanumeric)
                    });
                update.execute(params![
                    if mentioned { "true" } else { "false" },
                    revision,
                    language,
                    source,
                    target
                ])?;
                let Some(next) = rows.next()? else { break };
                row = next;
            }
            Ok(())
        },
    )
}

/// Conservative literal spans. A separator prevents a match across removed markup.
fn editorial_body(text: &str, syntax: &regex::Regex) -> String {
    let mut body = String::new();
    let mut closing = Vec::<String>::new();
    let mut cursor = 0;
    for token in syntax.find_iter(text) {
        if closing.is_empty() {
            body.push_str(&text[cursor..token.start()].to_lowercase());
            body.push('\0');
        }
        cursor = token.end();
        if token.as_str().starts_with("<!--") {
            continue;
        }
        let mark = token.as_str().to_ascii_lowercase();
        // Inside literal or extension elements, delimiters have no effect.
        if let Some(tag) = closing.last().filter(|s| s.starts_with("</")) {
            if mark.trim_end_matches('>').trim_end() == tag {
                closing.pop();
            }
            continue;
        }
        if closing.last() == Some(&mark) {
            closing.pop();
            continue;
        }
        match mark.as_str() {
            "{{" => closing.push("}}".into()),
            "[[" => closing.push("]]".into()),
            "[" => closing.push("]".into()),
            "{|" => closing.push("|}".into()),
            _ if mark.starts_with('<') && !mark.starts_with("</") && !mark.ends_with("/>") => {
                let name = mark[1..]
                    .split(|c: char| c.is_whitespace() || c == '>')
                    .next()
                    .unwrap_or("");
                if matches!(
                    name,
                    "ref"
                        | "references"
                        | "nowiki"
                        | "pre"
                        | "source"
                        | "syntaxhighlight"
                        | "math"
                        | "gallery"
                        | "code"
                        | "templatedata"
                        | "timeline"
                        | "graph"
                        | "score"
                ) {
                    closing.push(format!("</{name}"));
                }
            }
            _ => {}
        }
    }
    if closing.is_empty() {
        body.push_str(&text[cursor..].to_lowercase());
    }
    body
}

#[test]
fn shortlist_ranks_before_applying_limits_and_keeps_all_pairs() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE candidates(language,target,source,position,data);
        CREATE TEMP TABLE support(language,target,source,editions,first_edge);
        CREATE TABLE shortlists(language,target,source,position,evidence_position);",
    )
    .unwrap();
    for source in 1..=12 {
        db.execute(
            "INSERT INTO candidates VALUES('vec',20,?,?,?)",
            params![
                source,
                source,
                json!({"dead_end":source<=6,"local_mention":source==6||source==12}).to_string()
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO support VALUES('vec',20,?,?,0)",
            params![source, if source == 11 || source == 12 { 2 } else { 1 }],
        )
        .unwrap();
    }
    rank_shortlists(&db).unwrap();
    let mut query = db
        .prepare("SELECT source FROM shortlists ORDER BY position")
        .unwrap();
    let ids = query
        .query_map([], |row| row.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(ids, [12, 6, 11, 1, 2, 3, 4, 7, 8, 9]);
    assert_eq!(
        db.query_row("SELECT count(*) FROM candidates", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        12
    );
}

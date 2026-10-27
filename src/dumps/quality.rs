#[cfg(feature = "cli")]
use super::{
    file_signature, first_metadata, history_decimal, history_files, python_json, resolved_path,
    write_atomic, DumpClient, WikimediaHttp,
};
#[cfg(feature = "cli")]
use super::{DumpResult, SQL_TABLES};

/// Frozen Johnson V2 extractor. Its regex limitations are intentional: markup
/// in comments/nowiki counts, links can nest badly, and media is not expanded.
pub struct QualityExtractor {
    references: [regex::Regex; 2],
    headings: regex::Regex,
    categories: std::collections::HashSet<String>,
    media: std::collections::HashSet<String>,
}

impl QualityExtractor {
    pub fn new(
        language: &str,
        spec: &serde_json::Value,
        namespaces: &std::collections::HashMap<i32, String>,
    ) -> Result<Self, String> {
        let approved: serde_json::Value =
            serde_json::from_str(include_str!("../../config/quality.json"))
                .map_err(|e| e.to_string())?;
        if spec != &approved {
            return Err("Modello qualità non approvato".into());
        }
        let prefixes = |kind: &str| -> Result<std::collections::HashSet<String>, String> {
            let mut values = std::collections::HashSet::new();
            for list in [
                &spec[format!("{kind}_prefixes")],
                &spec[format!("{kind}_aliases")][language],
            ] {
                if let Some(list) = list.as_array() {
                    for value in list {
                        values.insert(value.as_str().ok_or("Alias qualità non valido")?.to_owned());
                    }
                }
            }
            Ok(values)
        };
        let mut categories = prefixes("category")?;
        let mut media = prefixes("media")?;
        for (key, name) in namespaces {
            if *key == 14 {
                categories.insert(name.clone());
            } else if matches!(key, -2 | 6) {
                media.insert(name.clone());
            }
        }
        let compile = |pattern: &str| regex::Regex::new(pattern).map_err(|e| e.to_string());
        Ok(Self {
            // Python's Unicode \s also contains the four information separators.
            references: [
                compile(r"(?im)<ref([\s\x1c-\x1f][^/>]*)?/>")?,
                compile(r"(?im)<ref([\s\x1c-\x1f][^/>]*)?>[\s\S]*?</ref>")?,
            ],
            headings: compile(r"(={2,})(.*?)(={2,})")?,
            categories,
            media,
        })
    }

    /// Feature order: UTF-8 bytes, references, headings, article links, media,
    /// categories. An empty supplied string is a known zero measurement.
    pub fn features(&self, text: &str) -> Result<[u64; 6], String> {
        let (mut links, mut categories, mut media) = (0_u64, 0_u64, 0_u64);
        let bytes = text.as_bytes();
        let mut next = 0_usize;
        let mut previous_empty = None;
        // Equivalent to (?<=\[\[)(.*?)(?=\]\]) without introducing a new
        // regex engine. Keep the delimiters unconsumed, including Python's
        // nonempty match at the same position immediately after an empty match.
        while next <= bytes.len() {
            let offset = next.saturating_sub(2);
            let Some(start) =
                bytes[offset..]
                    .windows(2)
                    .enumerate()
                    .find_map(|(position, pair)| {
                        (pair == b"[[" && offset + position + 2 >= next)
                            .then_some(offset + position + 2)
                    })
            else {
                break;
            };
            let close_from = start + usize::from(previous_empty == Some(start));
            let Some(end) = bytes
                .get(close_from..)
                .and_then(|tail| tail.windows(2).position(|pair| pair == b"]]"))
                .map(|end| close_from + end)
            else {
                break;
            };
            let prefix = text[start..end].split(':').next().unwrap_or("");
            links += 1;
            categories += u64::from(self.categories.contains(prefix));
            media += u64::from(self.media.contains(prefix));
            previous_empty = (start == end).then_some(start);
            next = end;
        }
        let article_links = links
            .checked_sub(categories + media)
            .ok_or("Namespace qualità sovrapposti")?;
        Ok([
            text.len() as u64,
            self.references
                .iter()
                .map(|regex| regex.find_iter(text).count() as u64)
                .sum(),
            self.headings
                .captures_iter(text)
                .filter(|capture| capture.get(1).is_some_and(|left| left.len() <= 3))
                .count() as u64,
            article_links,
            media,
            categories,
        ])
    }
}

#[cfg(feature = "cli")]
pub(super) type ContentNamespaces = std::collections::HashMap<i32, String>;

#[cfg(feature = "cli")]
pub(super) type ContentConsumer<'a> =
    dyn FnMut(ContentPage, &ContentNamespaces) -> DumpResult<()> + 'a;

#[cfg(feature = "cli")]
#[derive(Default)]
pub(super) struct ContentElement {
    name: String,
    namespace: String,
    attributes: std::collections::HashMap<String, String>,
    text: String,
    children: Vec<ContentElement>,
}

#[cfg(feature = "cli")]
impl ContentElement {
    fn child(&self, name: &str) -> Option<&Self> {
        self.children
            .iter()
            .find(|child| child.name == name && child.namespace == self.namespace)
    }
    fn value(&self, name: &str) -> Option<&str> {
        self.child(name).map(|child| child.text.as_str())
    }
    fn descendants<'a>(&'a self, name: &'a str) -> Box<dyn Iterator<Item = &'a Self> + 'a> {
        self.descendants_in(name, &self.namespace)
    }
    fn descendants_in<'a>(
        &'a self,
        name: &'a str,
        namespace: &'a str,
    ) -> Box<dyn Iterator<Item = &'a Self> + 'a> {
        Box::new(self.children.iter().flat_map(move |child| {
            std::iter::once(child)
                .filter(move |child| child.name == name && child.namespace == namespace)
                .chain(child.descendants_in(name, namespace))
        }))
    }
}

#[cfg(feature = "cli")]
pub(super) struct ContentPage {
    id: i64,
    revision: i64,
    namespace: i64,
    redirect: bool,
    text: Option<String>,
}

/// Parse at most one page tree at a time. Text never leaves transient memory.
#[cfg(feature = "cli")]
pub(super) fn scan_content(
    path: &std::path::Path,
    database: &str,
    mut consume: impl FnMut(ContentPage, &std::collections::HashMap<i32, String>) -> DumpResult<()>,
) -> DumpResult<()> {
    crate::runtime::phase("quality.xml_scan", || {
        crate::runtime::check_cancelled()?;
        use quick_xml::{events::Event, name::ResolveResult, reader::NsReader, XmlVersion};
        use std::io::BufReader;
        let mut reader = NsReader::from_reader(BufReader::new(bzip2::read::MultiBzDecoder::new(
            std::fs::File::open(path)?,
        )));
        reader.config_mut().enable_all_checks(true);
        let mut buffer = Vec::new();
        let mut stack = Vec::<ContentElement>::new();
        let mut depth = 0_usize;
        let mut root_namespace = None::<String>;
        let mut identified = false;
        let mut declaration_allowed = true;
        let mut namespaces = std::collections::HashMap::new();
        let complete = |element: ContentElement,
                        identified: &mut bool,
                        namespaces: &mut std::collections::HashMap<i32, String>,
                        consume: &mut ContentConsumer<'_>|
         -> DumpResult<()> {
            if element.name == "siteinfo" {
                let name = element.value("dbname").unwrap_or("");
                let base = element.value("base").unwrap_or("");
                let prefix = format!(
                    "https://{}.wikipedia.org/",
                    database
                        .strip_suffix("wiki")
                        .unwrap_or(database)
                        .replace('_', "-")
                );
                if name != database && (!name.is_empty() || !base.starts_with(&prefix)) {
                    return Err("Contenuto XML di una wiki diversa".into());
                }
                *identified = true;
                for namespace in element.descendants("namespace") {
                    if let Some(key) = namespace
                        .attributes
                        .get("key")
                        .filter(|key| matches!(key.as_str(), "-2" | "6" | "14"))
                        .and_then(|key| key.parse::<i32>().ok())
                    {
                        if !namespace.text.is_empty() {
                            namespaces.insert(key, namespace.text.clone());
                        }
                    }
                }
            } else if element.name == "page" {
                if !*identified {
                    return Err("Identità XML non verificata".into());
                }
                let id = history_decimal(element.value("id").unwrap_or(""))?
                    .ok_or("Identità pagina XML non valida")?;
                let namespace = element
                    .value("ns")
                    .ok_or("Identità pagina XML non valida")?;
                if history_decimal(namespace.trim_start_matches('-'))?.is_none() {
                    return Err("Identità pagina XML non valida".into());
                }
                let namespace = namespace.parse::<i64>()?;
                let revisions = element
                    .children
                    .iter()
                    .filter(|child| {
                        child.name == "revision" && child.namespace == element.namespace
                    })
                    .collect::<Vec<_>>();
                if revisions.len() != 1 {
                    return Err(
                        "Il contenuto corrente deve avere una sola revisione per pagina".into(),
                    );
                }
                let revision = revisions[0];
                let rid = history_decimal(revision.value("id").unwrap_or(""))?
                    .ok_or("Revisione XML senza ID")?;
                let mut text = revision.child("text");
                let mut model = revision.value("model");
                if text.is_none() {
                    for slot in revision.descendants("slot") {
                        if slot.attributes.get("role").is_some_and(|s| s == "main")
                            || slot.value("role") == Some("main")
                        {
                            text = slot.child("text");
                            model = slot.value("model");
                        }
                    }
                }
                let text = text
                    .filter(|text| {
                        !text.attributes.contains_key("deleted")
                            && matches!(model, None | Some("wikitext"))
                    })
                    .map(|text| text.text.clone());
                consume(
                    ContentPage {
                        id,
                        revision: rid,
                        namespace,
                        redirect: element.child("redirect").is_some(),
                        text,
                    },
                    namespaces,
                )?;
            }
            Ok(())
        };
        loop {
            crate::runtime::check_cancelled()?;
            let (resolved, event) = reader.read_resolved_event_into(&mut buffer)?;
            let namespace = match resolved {
                ResolveResult::Unbound => String::new(),
                ResolveResult::Bound(namespace) => namespace.as_ref().to_owned(),
                ResolveResult::Unknown(_) => return Err("Namespace XML non risolto".into()),
            };
            if matches!(event, Event::Decl(_)) && !declaration_allowed {
                return Err("Dichiarazione XML in posizione non valida".into());
            }
            declaration_allowed = false;
            match event {
                Event::Start(ref tag) | Event::Empty(ref tag) => {
                    let empty = matches!(event, Event::Empty(_));
                    let name = tag.local_name().as_ref().to_owned();
                    if depth == 0 {
                        if root_namespace.is_some() || name != "mediawiki" {
                            return Err("Formato XML contenuto non valido".into());
                        }
                        root_namespace = Some(namespace.clone());
                    }
                    let mut attributes = std::collections::HashMap::new();
                    for attribute in tag.attributes() {
                        let attribute = attribute?;
                        let value = attribute
                            .normalized_value(XmlVersion::Implicit1_0)?
                            .into_owned();
                        check_xml_characters(&value)?;
                        let raw = attribute.key.as_ref();
                        let key = if raw == "xmlns" || raw.starts_with("xmlns:") {
                            raw.to_owned()
                        } else {
                            match reader.resolver().resolve_attribute(attribute.key) {
                                (ResolveResult::Unbound, _) => raw.to_owned(),
                                (ResolveResult::Bound(namespace), name) => {
                                    format!("{{{}}}{}", namespace.as_ref(), name.as_ref())
                                }
                                (ResolveResult::Unknown(_), _) => {
                                    return Err("Namespace attributo XML non risolto".into())
                                }
                            }
                        };
                        if attributes.insert(key, value).is_some() {
                            return Err("Attributo XML duplicato".into());
                        }
                    }
                    if !stack.is_empty()
                        || (depth == 1
                            && root_namespace.as_ref() == Some(&namespace)
                            && matches!(name.as_str(), "page" | "siteinfo"))
                    {
                        let element = ContentElement {
                            name,
                            namespace,
                            attributes,
                            ..ContentElement::default()
                        };
                        if empty {
                            if let Some(parent) = stack.last_mut() {
                                parent.children.push(element);
                            } else {
                                complete(element, &mut identified, &mut namespaces, &mut consume)?;
                            }
                        } else {
                            stack.push(element);
                        }
                    }
                    if !empty {
                        depth += 1;
                    }
                }
                Event::End(_) => {
                    depth = depth.checked_sub(1).ok_or("XML con chiusura inattesa")?;
                    if !stack.is_empty() {
                        let element = stack.pop().ok_or("XML incompleto")?;
                        if let Some(parent) = stack.last_mut() {
                            parent.children.push(element);
                        } else {
                            complete(element, &mut identified, &mut namespaces, &mut consume)?;
                        }
                    }
                }
                Event::Text(text) => {
                    let text = text.xml_content(XmlVersion::Implicit1_0);
                    check_xml_characters(&text)?;
                    if depth == 0 && text.chars().any(|c| !matches!(c, '\t' | '\n' | '\r' | ' ')) {
                        return Err("Testo fuori dal documento XML".into());
                    }
                    if let Some(element) = stack.last_mut().filter(|e| e.children.is_empty()) {
                        element.text.push_str(&text);
                    }
                }
                Event::CData(text) => {
                    if depth == 0 {
                        return Err("CDATA fuori dal documento XML".into());
                    }
                    check_xml_characters(&text)?;
                    if let Some(element) = stack.last_mut().filter(|e| e.children.is_empty()) {
                        element
                            .text
                            .push_str(&text.xml_content(XmlVersion::Implicit1_0));
                    }
                }
                Event::GeneralRef(entity) => {
                    if depth == 0 {
                        return Err("Entità fuori dal documento XML".into());
                    }
                    let text = if let Some(character) = entity.resolve_char_ref()? {
                        character.to_string()
                    } else {
                        quick_xml::escape::resolve_predefined_entity(&entity)
                            .ok_or("Entità XML non risolta")?
                            .to_owned()
                    };
                    check_xml_characters(&text)?;
                    if let Some(element) = stack.last_mut().filter(|e| e.children.is_empty()) {
                        element.text.push_str(&text);
                    }
                }
                Event::DocType(_) => {
                    return Err("DTD non previsto nell'export XML ufficiale".into())
                }
                Event::Eof => break,
                _ => {}
            }
            buffer.clear();
        }
        if depth != 0 || !identified {
            return Err("Identità XML non verificata o documento incompleto".into());
        }
        Ok(())
    })
}

#[cfg(feature = "cli")]
pub(super) fn check_xml_characters(text: &str) -> DumpResult<()> {
    let valid = |c| {
        matches!(c,
            '\t'|'\n'|'\r'|'\u{20}'..='\u{d7ff}'|'\u{e000}'..='\u{fffd}'|'\u{10000}'..='\u{10ffff}'
        )
    };
    if !text.chars().all(valid) {
        return Err("Carattere XML non valido".into());
    }
    Ok(())
}

#[cfg(feature = "cli")]
#[derive(serde::Deserialize)]
pub(super) struct ContentManifest {
    language: String,
    database: String,
    source: String,
    complete: bool,
    content_release: String,
    #[serde(deserialize_with = "history_files")]
    files: Vec<(String, serde_json::Value)>,
}

#[cfg(feature = "cli")]
pub(super) fn flush_quality(
    connection: &rusqlite::Connection,
    batch: &mut Vec<(ContentPage, std::collections::HashMap<i32, String>)>,
    language: &str,
    spec: &serde_json::Value,
    name: &str,
    extractor: &mut Option<(std::collections::HashMap<i32, String>, QualityExtractor)>,
) -> DumpResult<()> {
    crate::runtime::phase("quality.features", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::params;
        if batch.is_empty() {
            return Ok(());
        }
        let marks = vec!["?"; batch.len()].join(",");
        let selected = connection
            .prepare(&format!(
                "SELECT id FROM quality_census WHERE id IN ({marks})"
            ))?
            .query_map(
                rusqlite::params_from_iter(batch.iter().map(|(page, _)| page.id)),
                |row| row.get::<_, i64>(0),
            )?
            .collect::<Result<std::collections::HashSet<_>, _>>()?;
        connection.execute_batch("BEGIN")?;
        let mut query =
            connection.prepare_cached("INSERT INTO quality_pages VALUES(?,?,?,?,?,?,?,?,?,?)")?;
        for (page, namespaces) in batch.drain(..) {
            if !selected.contains(&page.id) {
                continue;
            }
            if extractor.as_ref().map(|(previous, _)| previous) != Some(&namespaces) {
                *extractor = Some((
                    namespaces.clone(),
                    QualityExtractor::new(language, spec, &namespaces)?,
                ));
            }
            let extractor = extractor
                .as_ref()
                .map(|(_, extractor)| extractor)
                .ok_or("Namespace qualità non disponibili")?;
            let features = page
                .text
                .as_deref()
                .map(|text| extractor.features(text))
                .transpose()?;
            let encoded = features
                .map(|features| python_json(&serde_json::json!(features)))
                .transpose()?;
            let x = crate::dashboard::quality_transform(features.unwrap_or([0; 6]));
            query.execute(params![
                page.id,
                page.revision,
                name,
                encoded,
                x[0],
                x[1],
                x[2],
                x[3],
                x[4],
                x[5]
            ])?;
        }
        connection.execute_batch("COMMIT")?;
        Ok(())
    })
}

/// Calculate the approved Johnson V2 supplement from a complete SQL census and
/// verified official XML partitions, retaining no article text in SQLite.
#[cfg(feature = "cli")]
pub fn infer_quality(
    raw_path: &std::path::Path,
    manifest_path: &std::path::Path,
    output: &std::path::Path,
    work_dir: &std::path::Path,
    spec_path: &std::path::Path,
) -> DumpResult<serde_json::Value> {
    crate::runtime::phase("quality.run", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::{params, OptionalExtension};
        use serde_json::{json, Value};
        use std::io::Write;
        let graph = first_metadata(raw_path)?;
        let language = graph["language"]
            .as_str()
            .ok_or("Lingua del grafo mancante")?;
        let database = crate::wiki_database(language, graph["database"].as_str())?;
        let manifest: ContentManifest = serde_json::from_slice(&std::fs::read(manifest_path)?)?;
        if graph["source"] != format!("Wikimedia SQL dumps / {database}")
            || graph["complete"] != true
            || manifest.source != "mediawiki_content_current"
            || !manifest.complete
            || manifest.files.is_empty()
            || manifest.database != database
            || manifest.language != language
        {
            return Err("Manifest contenuto non corrispondente al grafo".into());
        }
        let spec: Value = serde_json::from_slice(&std::fs::read(spec_path)?)?;
        QualityExtractor::new(language, &spec, &std::collections::HashMap::new())?;
        let hash = |path: &std::path::Path| -> DumpResult<String> {
            Ok(
                DumpClient::<WikimediaHttp>::verified_sha256(path, None, None)?
                    .ok_or("File qualità mancante")?,
            )
        };
        let identity = json!({"input_file_sha256":hash(raw_path)?,"manifest_sha256":hash(manifest_path)?,"model":spec,"quality_revision_rule":"selected_official_xml_revision_v1","dump_manifest_sha256":graph["dump_manifest_sha256"],"graph_finished_at":graph["finished_at"]});
        let release = graph["sql_release"]
            .as_str()
            .ok_or("Release SQL mancante")?;
        let sql_path = work_dir.join(format!("{database}-{release}.sqlite"));
        if !sql_path.is_file() {
            return Err("Staging SQL mancante per qualità".into());
        }
        let stage_path = work_dir.join(format!("{database}-{release}-quality.sqlite"));
        let sql_manifest_path = std::path::Path::new(
            graph["dump_manifest"]
                .as_str()
                .ok_or("Manifest SQL mancante")?,
        );
        let resolved_output = resolved_path(output)?;
        for path in [
            raw_path,
            manifest_path,
            sql_path.as_path(),
            stage_path.as_path(),
            sql_manifest_path,
            spec_path,
        ] {
            if resolved_output == resolved_path(path)? {
                return Err("Output qualità sovrapposto agli input".into());
            }
        }
        for (_, record) in &manifest.files {
            let path =
                std::path::Path::new(record["path"].as_str().ok_or("Percorso XML mancante")?);
            if resolved_output == resolved_path(path)? {
                return Err("Output qualità sovrapposto agli input".into());
            }
        }
        let sql = rusqlite::Connection::open_with_flags(
            &sql_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let sql_manifest: Value = serde_json::from_slice(&std::fs::read(sql_manifest_path)?)?;
        if hash(sql_manifest_path)? != graph["dump_manifest_sha256"] {
            return Err("Manifest SQL modificato".into());
        }
        for table in SQL_TABLES {
            let imported: Option<String> = sql
                .query_row(
                    "SELECT sha256 FROM imported WHERE table_name=?",
                    [table],
                    |r| r.get(0),
                )
                .optional()?;
            if imported.as_deref() != sql_manifest["files"][table]["sha256_local"].as_str() {
                return Err("Staging SQL incompleto per qualità".into());
            }
        }
        let db = rusqlite::Connection::open(&stage_path)?;
        db.execute_batch("PRAGMA temp_store=FILE; PRAGMA cache_size=-8192;
        CREATE TABLE IF NOT EXISTS quality_context(identity TEXT);
        CREATE TABLE IF NOT EXISTS quality_parts(name TEXT PRIMARY KEY,digest TEXT,size INTEGER,mtime_ns INTEGER);
        CREATE TABLE IF NOT EXISTS quality_pages(id INTEGER PRIMARY KEY,revision INTEGER,file TEXT,features TEXT,x0 REAL,x1 REAL,x2 REAL,x3 REAL,x4 REAL,x5 REAL);
        CREATE TABLE IF NOT EXISTS quality_census(id INTEGER PRIMARY KEY,revision INTEGER);")?;
        let encoded = python_json(&identity)?;
        let previous: Option<String> = db
            .query_row("SELECT identity FROM quality_context", [], |r| r.get(0))
            .optional()?;
        if previous.as_deref() != Some(&encoded) {
            db.execute_batch("BEGIN; DELETE FROM quality_context; DELETE FROM quality_parts; DELETE FROM quality_pages; DELETE FROM quality_census")?;
            db.execute("INSERT INTO quality_context VALUES(?)", [encoded])?;
            let mut rows =
                sql.prepare("SELECT id,revision FROM dump_page WHERE ns=0 AND redirect=0")?;
            let mut rows = rows.query([])?;
            let mut insert = db.prepare("INSERT INTO quality_census VALUES(?,?)")?;
            while let Some(row) = rows.next()? {
                insert.execute(params![
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?
                ])?;
            }
            db.execute_batch("COMMIT")?;
        }
        let census = db.query_row("SELECT COUNT(*) FROM quality_census", [], |r| {
            r.get::<_, u64>(0)
        })?;
        if census == 0 {
            return Err("Censimento qualità vuoto".into());
        }
        for (name, record) in &manifest.files {
            let path =
                std::path::Path::new(record["path"].as_str().ok_or("Percorso XML mancante")?);
            let signature = file_signature(path)?;
            let digest = record["sha256_local"]
                .as_str()
                .ok_or("Checksum XML mancante")?;
            if Some(signature.0) != record["bytes"].as_u64()
                || record["sha256_local"] != record["sha256_publisher"]
            {
                return Err("Checksum contenuto XML errato".into());
            }
            let previous = db
                .query_row(
                    "SELECT digest,size,mtime_ns FROM quality_parts WHERE name=?",
                    [name],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, u64>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?;
            if previous == Some((digest.to_owned(), signature.0, signature.1)) {
                eprintln!("CHECKPOINT XML {name}: verified_reuse");
                continue;
            }
            if hash(path)? != digest {
                return Err("Checksum contenuto XML errato".into());
            }
            db.execute("DELETE FROM quality_pages WHERE file=?", [name])?;
            let mut batch = Vec::new();
            let mut bytes = 0;
            let mut extractor = None;
            scan_content(path, &database, |page, namespaces| {
                if page.namespace == 0 && !page.redirect {
                    bytes += page.text.as_ref().map_or(0, String::len);
                    batch.push((page, namespaces.clone()));
                    if batch.len() == 512 || bytes >= 8 * 1024 * 1024 {
                        flush_quality(&db, &mut batch, language, &spec, name, &mut extractor)?;
                        bytes = 0;
                    }
                }
                Ok(())
            })?;
            flush_quality(&db, &mut batch, language, &spec, name, &mut extractor)?;
            if file_signature(path)? != signature {
                return Err("Contenuto XML modificato durante elaborazione".into());
            }
            db.execute(
                "INSERT OR REPLACE INTO quality_parts VALUES(?,?,?,?)",
                params![name, digest, signature.0, signature.1],
            )?;
        }
        let n = db.query_row(
            "SELECT COUNT(*) FROM quality_pages WHERE features IS NOT NULL",
            [],
            |r| r.get::<_, u64>(0),
        )?;
        let (maxima, normalizers) = crate::runtime::phase(
            "quality.percentiles",
            || -> DumpResult<([f64; 6], Vec<Value>)> {
                crate::runtime::check_cancelled()?;
                let mut maxima = [0.0; 6];
                let mut normalizers = Vec::with_capacity(6);
                for (index, maximum) in maxima.iter_mut().enumerate() {
                    let minimum = spec["minimums"][index]
                        .as_f64()
                        .ok_or("Normalizzatore qualità mancante")?;
                    let mut percentile = 0.0;
                    if n > 0 {
                        let rank = (n - 1) as f64
                            * spec["percentile"]
                                .as_f64()
                                .ok_or("Percentile qualità mancante")?;
                        let offset = rank.floor() as u64;
                        let pair=db.prepare(&format!("SELECT x{index} FROM quality_pages WHERE features IS NOT NULL ORDER BY x{index} LIMIT 2 OFFSET ?"))?.query_map([offset],|r|r.get::<_,f64>(0))?.collect::<Result<Vec<_>,_>>()?;
                        let first = pair
                            .first()
                            .copied()
                            .ok_or("Percentile qualità incompleto")?;
                        percentile = first
                            + (rank - offset as f64)
                                * (pair.last().copied().unwrap_or(first) - first);
                    }
                    *maximum = minimum.max(percentile);
                    // Python max retains its first argument on ties, including its
                    // JSON integer type. This is part of supplement digest identity.
                    normalizers.push(if minimum >= percentile {
                        spec["minimums"][index].clone()
                    } else {
                        json!(percentile)
                    });
                }
                Ok((maxima, normalizers))
            },
        )?;
        let valid=db.query_row("SELECT COUNT(*) FROM quality_census c JOIN quality_pages p ON c.id=p.id WHERE p.features IS NOT NULL",[],|r|r.get::<_,u64>(0))?;
        let (same,different,unavailable)=db.query_row("SELECT SUM(CASE WHEN p.revision=c.revision THEN 1 ELSE 0 END),SUM(CASE WHEN p.revision IS NOT NULL AND p.revision!=c.revision THEN 1 ELSE 0 END),SUM(CASE WHEN p.revision IS NULL THEN 1 ELSE 0 END) FROM quality_census c LEFT JOIN quality_pages p ON p.id=c.id",[],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,u64>(1)?,r.get::<_,u64>(2)?)))?;
        let mut metadata = identity;
        let content_files = manifest
            .files
            .iter()
            .map(|(name, r)| (name.clone(), r["sha256_local"].clone()))
            .collect::<serde_json::Map<_, _>>();
        let extra = json!({"language":language,"database":database,"source":"Johnson V2 / mediawiki_content_current","sql_release":release,
        "content_release":manifest.content_release,"complete":true,"census_page_count":census,"scored_pages":valid,"missing_pages":census-valid,
        "same_revision_pages":same,"different_revision_pages":different,"unavailable_revision_pages":unavailable,"normalizers":normalizers,"content_files":content_files});
        metadata
            .as_object_mut()
            .ok_or("Metadati qualità non validi")?
            .extend(
                extra
                    .as_object()
                    .ok_or("Metadati qualità non validi")?
                    .clone(),
            );
        write_atomic(output, |file| {
            let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file);
            writer.write_all(b"{\"metadata\":")?;
            serde_json::to_writer(&mut writer, &metadata)?;
            writer.write_all(b",\"pages\":[")?;
            let mut query=db.prepare("SELECT c.id,c.revision,p.revision,p.features FROM quality_census c LEFT JOIN quality_pages p ON p.id=c.id ORDER BY c.id")?;
            let mut rows = query.query([])?;
            let mut first = true;
            loop {
                crate::runtime::check_cancelled()?;
                let mut batch = Vec::new();
                while batch.len() < 512 {
                    let Some(row) = rows.next()? else {
                        break;
                    };
                    let features = row
                        .get::<_, Option<String>>(3)?
                        .map(|s| serde_json::from_str::<[u64; 6]>(&s))
                        .transpose()?;
                    batch.push((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        features,
                    ));
                }
                if batch.is_empty() {
                    break;
                }
                let scores = crate::dashboard::quality_scores(
                    &batch
                        .iter()
                        .map(|row| row.3.unwrap_or([0; 6]))
                        .collect::<Vec<_>>(),
                    maxima,
                )?;
                for ((page, revision, actual, features), score) in batch.into_iter().zip(scores) {
                    let relation = if actual.is_none() {
                        "unavailable"
                    } else if revision == actual {
                        "same"
                    } else {
                        "different"
                    };
                    let status = if actual.is_none() {
                        "missing_current_page"
                    } else if features.is_none() {
                        "missing_wikitext"
                    } else {
                        "scored"
                    };
                    let record = json!({"page_id":page,"revision_id":revision,"content_revision_id":actual,"quality_revision_relation":relation,
                    "quality_score":if status=="scored" {Some(score)} else {None},"quality_status":status});
                    if !first {
                        writer.write_all(b",")?;
                    }
                    first = false;
                    serde_json::to_writer(&mut writer, &record)?;
                }
            }
            writer.write_all(b"]}\n")?;
            writer.flush()?;
            Ok(())
        })?;
        Ok(metadata)
    })
}

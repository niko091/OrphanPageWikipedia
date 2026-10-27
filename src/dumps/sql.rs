#[cfg(feature = "cli")]
use super::{
    history_timestamp, import_history, resolved_path, write_atomic, DumpClient, HistoryOptions,
    WikimediaHttp,
};
#[cfg(feature = "cli")]
use super::{DumpResult, SQL_TABLES};

pub fn parse_insert(statement: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    let offset = if statement.trim_start().starts_with('(') {
        statement.len() - statement.trim_start().len()
    } else {
        let marker = " VALUES";
        statement
            .find(marker)
            .ok_or_else(|| "SQL dump INSERT without VALUES".to_owned())?
            + marker.len()
    };
    let bytes = statement.as_bytes();
    let mut position = offset;
    let mut rows = Vec::new();
    while position < bytes.len() {
        match bytes[position] {
            b'(' => position += 1,
            b';' | b'\n' | b'\r' | b' ' => {
                position += 1;
                continue;
            }
            b',' => {
                position += 1;
                continue;
            }
            _ => return Err("invalid SQL dump tuple boundary".to_owned()),
        }
        if position >= bytes.len() {
            break;
        }
        let mut row = Vec::new();
        loop {
            if position >= bytes.len() {
                return Err("truncated SQL dump tuple".to_owned());
            }
            if bytes[position] == b'\'' {
                position += 1;
                let mut value = Vec::new();
                let mut closed = false;
                while position < bytes.len() {
                    match bytes[position] {
                        b'\\' => {
                            position += 1;
                            let escaped = *bytes
                                .get(position)
                                .ok_or_else(|| "truncated SQL escape".to_owned())?;
                            value.push(match escaped {
                                b'0' => 0,
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                b'b' => 8,
                                b'Z' => 26,
                                other => other,
                            });
                            position += 1;
                        }
                        b'\'' => {
                            position += 1;
                            closed = true;
                            break;
                        }
                        byte => {
                            value.push(byte);
                            position += 1;
                        }
                    }
                }
                if !closed {
                    return Err("unterminated SQL dump string".to_owned());
                }
                // SQL varbinary fields such as category sort keys can contain
                // arbitrary bytes. The importer never uses those columns as
                // identities; relevant page/link titles must be valid UTF-8.
                row.push(Some(String::from_utf8_lossy(&value).into_owned()));
            } else {
                let start = position;
                while position < bytes.len() && !matches!(bytes[position], b',' | b')') {
                    position += 1;
                }
                let raw = std::str::from_utf8(&bytes[start..position])
                    .map_err(|_| "invalid SQL dump scalar".to_owned())?
                    .trim();
                if raw.is_empty() {
                    return Err("empty SQL dump scalar".to_owned());
                }
                row.push((raw != "NULL").then(|| raw.to_owned()));
            }
            match bytes.get(position) {
                Some(b',') => position += 1,
                Some(b')') => {
                    position += 1;
                    break;
                }
                _ => return Err("invalid SQL dump field boundary".to_owned()),
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(feature = "cli")]
pub(super) fn read_sql(
    path: &std::path::Path,
    table: &str,
    mut consume: impl FnMut(&[String], Vec<Vec<Option<String>>>) -> DumpResult<()>,
) -> DumpResult<()> {
    crate::runtime::phase("sql.scan", || {
        crate::runtime::check_cancelled()?;
        use std::io::BufRead;
        let file = std::fs::File::open(path)?;
        let mut stream = std::io::BufReader::new(flate2::read::MultiGzDecoder::new(file));
        let header = format!("INSERT INTO `{table}` VALUES");
        let mut columns = Vec::new();
        let mut statement = String::new();
        let mut inside = false;
        let mut bytes = Vec::new();
        let (mut scanned_rows, mut scanned_bytes) = (0u64, 0u64);
        loop {
            crate::runtime::check_cancelled()?;
            bytes.clear();
            if stream.read_until(b'\n', &mut bytes)? == 0 {
                break;
            }
            let line = String::from_utf8_lossy(&bytes);
            scanned_bytes += bytes.len() as u64;
            if !inside && line.starts_with("  `") {
                if let Some(end) = line[3..].find('`') {
                    if line[3 + end + 1..]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace)
                    {
                        columns.push(line[3..3 + end].to_owned());
                    }
                }
            }
            if line.starts_with(&header) {
                if columns.is_empty() {
                    return Err(format!("Schema SQL assente: {table}").into());
                }
                statement.clear();
                statement.push_str(&line);
                inside = true;
            } else if inside {
                statement.push_str(&line);
            }
            if inside && line.trim_end().ends_with(';') {
                let records = parse_insert(&statement)?;
                scanned_rows += records.len() as u64;
                consume(&columns, records)?;
                statement.clear();
                inside = false;
            }
        }
        if inside {
            return Err(format!("INSERT SQL troncato: {table}").into());
        }
        crate::runtime::phase_counts(Some(scanned_rows), Some(scanned_bytes));
        Ok(())
    })
}

#[cfg(feature = "cli")]
pub(super) fn load_sql_table(
    connection: &rusqlite::Connection,
    table: &str,
    path: &std::path::Path,
    digest: &str,
) -> DumpResult<()> {
    crate::runtime::phase("sql.table", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::{params, types::Value, OptionalExtension};
        let (target, requirements, placeholders): (&str, &[&str], &str) = match table {
            "page" => (
                "dump_page",
                &[
                    "page_id",
                    "page_namespace",
                    "page_title",
                    "page_is_redirect",
                    "page_len",
                    "page_latest",
                ],
                "?,?,?,?,?,?",
            ),
            "linktarget" => (
                "dump_target",
                &["lt_id", "lt_namespace", "lt_title"],
                "?,?,?",
            ),
            "pagelinks" => (
                "dump_link",
                &["pl_from", "pl_from_namespace", "pl_target_id"],
                "?,?",
            ),
            "redirect" => (
                "dump_redirect",
                &["rd_from", "rd_namespace", "rd_title", "rd_interwiki"],
                "?,?,?,?",
            ),
            "page_props" => (
                "dump_prop",
                &["pp_page", "pp_propname", "pp_value"],
                "?,?,?",
            ),
            "categorylinks" => ("dump_category", &["cl_from"], "?,?,?"),
            "user_groups" => ("dump_group", &["ug_user", "ug_group", "ug_expiry"], "?,?,?"),
            _ => return Err("Tabella SQL non prevista".into()),
        };
        let old = connection
            .query_row(
                "SELECT sha256 FROM imported WHERE table_name=?",
                [table],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if old.as_deref() == Some(digest) {
            println!("CHECKPOINT SQL {table}: verified_reuse");
            return Ok(());
        }
        {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute("DELETE FROM imported WHERE table_name=?", [table])?;
            transaction.execute(&format!("DELETE FROM {target}"), [])?;
            transaction.commit()?;
        }
        let mut transaction = Some(connection.unchecked_transaction()?);
        let mut schema: Option<Vec<String>> = None;
        let mut count = 0u64;
        let insert = format!("INSERT INTO {target} VALUES({placeholders})");
        read_sql(path, table, |columns, rows| {
            if let Some(previous) = &schema {
                if previous != columns {
                    return Err(format!("Schema variato durante {table}").into());
                }
            } else {
                let mut missing = requirements
                    .iter()
                    .filter(|name| !columns.iter().any(|column| column == **name))
                    .copied()
                    .collect::<Vec<_>>();
                missing.sort_unstable();
                if !missing.is_empty() {
                    return Err(format!("Colonne del dump mancanti: {}", missing.join(", ")).into());
                }
                if table == "categorylinks"
                    && !columns
                        .iter()
                        .any(|name| name == "cl_target_id" || name == "cl_to")
                {
                    return Err("Manca la destinazione delle categorie".into());
                }
                schema = Some(columns.to_vec());
            }
            let indices = columns
                .iter()
                .enumerate()
                .map(|(index, name)| (name.as_str(), index))
                .collect::<std::collections::HashMap<_, _>>();
            let start = count;
            {
                let mut statement = transaction
                    .as_ref()
                    .ok_or("Transazione SQL non disponibile")?
                    .prepare_cached(&insert)?;
                for row in rows {
                    if row.len() != columns.len() {
                        return Err(format!("Numero di colonne errato in {table}").into());
                    }
                    let field = |name: &str| -> DumpResult<Option<&str>> {
                        Ok(row
                            .get(
                                *indices
                                    .get(name)
                                    .ok_or_else(|| format!("Colonna mancante: {name}"))?,
                            )
                            .ok_or("Riga SQL incompleta")?
                            .as_deref())
                    };
                    let text = |name: &str| -> DumpResult<Value> {
                        Ok(field(name)?
                            .map(|value| Value::Text(value.to_owned()))
                            .unwrap_or(Value::Null))
                    };
                    let integer = |name: &str| -> DumpResult<Value> {
                        Ok(Value::Integer(
                            field(name)?
                                .ok_or_else(|| format!("Intero SQL mancante: {name}"))?
                                .parse()?,
                        ))
                    };
                    let title = |name: &str, validate: bool| -> DumpResult<Value> {
                        let value = field(name)?.ok_or("Titolo SQL mancante")?;
                        if validate && value.contains('\u{fffd}') {
                            return Err("Titolo SQL non decodificabile".into());
                        }
                        Ok(Value::Text(value.replace('_', " ")))
                    };
                    let selected = match table {
                        "page" => vec![
                            integer("page_id")?,
                            integer("page_namespace")?,
                            title("page_title", true)?,
                            integer("page_is_redirect")?,
                            integer("page_len")?,
                            integer("page_latest")?,
                        ],
                        "linktarget" => vec![
                            integer("lt_id")?,
                            integer("lt_namespace")?,
                            title("lt_title", true)?,
                        ],
                        "pagelinks" => {
                            if field("pl_from_namespace")? != Some("0") {
                                continue;
                            }
                            vec![integer("pl_from")?, integer("pl_target_id")?]
                        }
                        "redirect" => vec![
                            integer("rd_from")?,
                            integer("rd_namespace")?,
                            title("rd_title", false)?,
                            text("rd_interwiki")?,
                        ],
                        "page_props" => {
                            if !matches!(
                                field("pp_propname")?,
                                Some("wikibase_item" | "hiddencat" | "disambiguation")
                            ) {
                                continue;
                            }
                            vec![integer("pp_page")?, text("pp_propname")?, text("pp_value")?]
                        }
                        "categorylinks" => {
                            let target = if indices.contains_key("cl_target_id")
                                && field("cl_target_id")?.is_some_and(|value| !value.is_empty())
                            {
                                integer("cl_target_id")?
                            } else {
                                Value::Null
                            };
                            let label = if indices.contains_key("cl_to")
                                && field("cl_to")?.is_some_and(|value| !value.is_empty())
                            {
                                title("cl_to", false)?
                            } else {
                                Value::Null
                            };
                            vec![integer("cl_from")?, target, label]
                        }
                        _ => vec![integer("ug_user")?, text("ug_group")?, text("ug_expiry")?],
                    };
                    statement.execute(rusqlite::params_from_iter(selected))?;
                    count += 1;
                }
            }
            if count != 0 && count % 1_000_000 < count - start {
                transaction
                    .take()
                    .ok_or("Transazione SQL non disponibile")?
                    .commit()?;
                transaction = Some(connection.unchecked_transaction()?);
                println!("Dump {table}: {count} righe");
            }
            Ok(())
        })?;
        transaction
            .take()
            .ok_or("Transazione SQL non disponibile")?
            .commit()?;
        let checkpoint = connection.unchecked_transaction()?;
        checkpoint.execute("INSERT INTO imported VALUES(?,?)", params![table, digest])?;
        checkpoint.commit()?;
        println!("Dump {table}: {count} righe acquisite");
        Ok(())
    })
}

#[cfg(feature = "cli")]
pub(super) fn next_relation(rows: &mut rusqlite::Rows<'_>) -> DumpResult<Option<(i64, String)>> {
    rows.next()?
        .map(|row| Ok((row.get(0)?, row.get(1)?)))
        .transpose()
}

// Graph byte checksums are inputs to later stages. Keep the reference field
// order and separators as well as its complete scientific records.
#[cfg(feature = "cli")]
pub(super) struct GraphFormatter;

#[cfg(feature = "cli")]
impl serde_json::ser::Formatter for GraphFormatter {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }
    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }
    fn begin_object_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        writer.write_all(b": ")
    }
}

#[cfg(feature = "cli")]
#[derive(Clone, Copy)]
pub(super) enum GraphObject {
    Metadata,
    Features,
    ItemMapping,
    Page,
}

#[cfg(feature = "cli")]
pub(super) struct GraphRecord<'a> {
    value: &'a serde_json::Value,
    kind: GraphObject,
}

#[cfg(feature = "cli")]
impl serde::Serialize for GraphRecord<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeMap};
        let fields: &[&str] = match self.kind {
            GraphObject::Metadata => &[
                "language",
                "database",
                "source",
                "sql_release",
                "dump_manifest_sha256",
                "complete",
                "demo",
                "started_at",
                "finished_at",
                "temporal_consistency",
                "creation_date_rule",
                "creator_collection_status",
                "article_features",
                "item_mapping",
                "dump_manifest",
            ],
            GraphObject::Features => &[
                "age",
                "length",
                "topic_inputs",
                "topics",
                "quality",
                "pageviews",
                "biographical_gender",
                "bot_at_creation",
            ],
            GraphObject::ItemMapping => &["source", "complete"],
            GraphObject::Page => &[
                "page_id",
                "title",
                "length_bytes",
                "revision_id",
                "wikibase_item",
                "created_at",
                "page_created_at",
                "first_public_revision_at",
                "creation_date_source",
                "creation_date_status",
                "links",
                "categories",
            ],
        };
        let object = self
            .value
            .as_object()
            .filter(|o| o.len() == fields.len())
            .ok_or_else(|| S::Error::custom("Struttura del record del grafo non valida"))?;
        let mut map = serializer.serialize_map(Some(fields.len()))?;
        for key in fields {
            let value = object
                .get(*key)
                .ok_or_else(|| S::Error::custom("Campo del grafo mancante"))?;
            let nested = match (self.kind, *key) {
                (GraphObject::Metadata, "article_features") => Some(GraphObject::Features),
                (GraphObject::Metadata, "item_mapping") => Some(GraphObject::ItemMapping),
                _ => None,
            };
            if let Some(kind) = nested {
                map.serialize_entry(key, &GraphRecord { value, kind })?;
            } else {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}

#[cfg(feature = "cli")]
pub(super) fn write_graph_json(
    value: &impl serde::Serialize,
    writer: &mut impl std::io::Write,
) -> DumpResult<()> {
    value.serialize(&mut serde_json::Serializer::with_formatter(
        writer,
        GraphFormatter,
    ))?;
    Ok(())
}

/// Import verified SQL archives using the existing table and checkpoint contracts.
/// The caller owns separate output/staging paths; collected inputs are read only.
#[cfg(feature = "cli")]
pub fn import_graph(
    manifest_path: &std::path::Path,
    output: &std::path::Path,
    work_dir: &std::path::Path,
) -> DumpResult<usize> {
    crate::runtime::phase("import.sql", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::params;
        use serde_json::{json, Value};
        use std::io::Write;
        let manifest: Value = serde_json::from_slice(&std::fs::read(manifest_path)?)?;
        let language = manifest["language"]
            .as_str()
            .ok_or("Lingua del manifest mancante")?;
        let database = crate::wiki_database(language, manifest["database"].as_str())?;
        if manifest["database"].as_str() != Some(&database) {
            return Err("Identità della wiki nel manifest non valida".into());
        }
        let release = manifest["sql_release"]
            .as_str()
            .ok_or("Release SQL mancante")?;
        if release.len() != 8
            || !release.starts_with("20")
            || !release.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("Data SQL richiesta in formato YYYYMMDD".into());
        }
        let stage = work_dir.join(format!("{database}-{release}.sqlite"));
        let resolved_output = resolved_path(output)?;
        if resolved_output == resolved_path(manifest_path)?
            || resolved_output == resolved_path(&stage)?
        {
            return Err("Output e input/staging devono essere distinti".into());
        }
        for table in SQL_TABLES {
            let path = manifest["files"][table]["path"]
                .as_str()
                .ok_or_else(|| format!("Percorso SQL mancante: {table}"))?;
            if resolved_output == resolved_path(std::path::Path::new(path))? {
                return Err("Output e input/staging devono essere distinti".into());
            }
        }
        std::fs::create_dir_all(work_dir)?;
        let connection = rusqlite::Connection::open(&stage)?;
        connection.execute_batch("PRAGMA temp_store=FILE; PRAGMA cache_size=-8192;
        CREATE TABLE IF NOT EXISTS imported(table_name TEXT PRIMARY KEY, sha256 TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS dump_page(id INTEGER PRIMARY KEY, ns INTEGER NOT NULL,title TEXT NOT NULL, redirect INTEGER NOT NULL, length INTEGER NOT NULL, revision INTEGER);
        CREATE TABLE IF NOT EXISTS dump_target(id INTEGER PRIMARY KEY, ns INTEGER NOT NULL,title TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS dump_link(source_id INTEGER NOT NULL,target_id INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS dump_redirect(page_id INTEGER NOT NULL, ns INTEGER NOT NULL,title TEXT NOT NULL,interwiki TEXT);
        CREATE TABLE IF NOT EXISTS dump_prop(page_id INTEGER NOT NULL,name TEXT NOT NULL,value TEXT);
        CREATE TABLE IF NOT EXISTS dump_category(source_id INTEGER NOT NULL,target_id INTEGER,title TEXT);
        CREATE TABLE IF NOT EXISTS dump_group(user_id INTEGER NOT NULL,name TEXT NOT NULL,expiry TEXT);")?;
        for table in SQL_TABLES {
            let record = &manifest["files"][table];
            let path =
                std::path::Path::new(record["path"].as_str().ok_or("Percorso SQL mancante")?);
            let digest = record["sha256_local"]
                .as_str()
                .ok_or("Checksum SQL mancante")?;
            if !path.is_file()
                || path.metadata()?.len()
                    != record["bytes"].as_u64().ok_or("Dimensione SQL mancante")?
                || DumpClient::<WikimediaHttp>::verified_sha256(path, None, Some(digest))?.is_none()
            {
                return Err(format!("Dump modificato o incompleto: {table}").into());
            }
            load_sql_table(&connection, table, path, digest)?;
        }
        connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS dump_link_source ON dump_link(source_id);
        CREATE INDEX IF NOT EXISTS dump_prop_page_name ON dump_prop(page_id,name);
        CREATE INDEX IF NOT EXISTS dump_page_ns_title ON dump_page(ns,title);
        CREATE INDEX IF NOT EXISTS dump_category_source ON dump_category(source_id);",
        )?;
        let expected = connection.query_row(
            "SELECT COUNT(*) FROM dump_page WHERE ns=0 AND redirect=0",
            [],
            |row| row.get::<_, usize>(0),
        )?;
        if expected == 0 {
            return Err("Il dump non contiene voci".into());
        }
        connection.execute_batch(
        "DROP TABLE IF EXISTS pages; DROP TABLE IF EXISTS links; DROP TABLE IF EXISTS categories;
        CREATE TABLE pages(id INTEGER PRIMARY KEY,data TEXT NOT NULL);
        CREATE TABLE links(id INTEGER,target TEXT);
        CREATE TABLE categories(id INTEGER,target TEXT);",
    )?;
        let qids = connection
            .prepare("SELECT page_id,value FROM dump_prop WHERE name='wikibase_item'")?
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<std::collections::HashMap<_, _>, _>>()?;
        {
            let transaction = connection.unchecked_transaction()?;
            let mut source = transaction.prepare(
            "SELECT id,title,length,revision FROM dump_page WHERE ns=0 AND redirect=0 ORDER BY id",
        )?;
            let mut insert = transaction.prepare("INSERT OR REPLACE INTO pages VALUES(?,?)")?;
            let mut rows = source.query([])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let item = qids
                    .get(&id)
                    .and_then(Option::as_deref)
                    .filter(|value| crate::valid_item(value));
                let page = json!({"page_id":id,"title":row.get::<_,String>(1)?,"length_bytes":row.get::<_,i64>(2)?,"revision_id":row.get::<_,Option<i64>>(3)?,"wikibase_item":item,"created_at":null,"page_created_at":null,"first_public_revision_at":null,"creation_date_source":null,"creation_date_status":"dump_history_not_joined"});
                insert.execute(params![id, serde_json::to_string(&page)?])?;
            }
            drop(rows);
            drop(source);
            drop(insert);
            transaction.commit()?;
        }
        connection.execute_batch("INSERT INTO links SELECT l.source_id,t.title FROM dump_link l JOIN dump_target t ON t.id=l.target_id
        JOIN dump_page p ON p.id=l.source_id WHERE t.ns=0 AND p.ns=0 AND p.redirect=0;
        INSERT INTO categories SELECT c.source_id,COALESCE(t.title,c.title) FROM dump_category c LEFT JOIN dump_target t ON t.id=c.target_id
        JOIN dump_page p ON p.id=c.source_id WHERE p.ns=0 AND p.redirect=0 AND (t.ns=14 OR c.title IS NOT NULL)
        AND NOT EXISTS (SELECT 1 FROM dump_page h JOIN dump_prop hp ON hp.page_id=h.id WHERE h.ns=14 AND hp.name='hiddencat' AND h.title=COALESCE(t.title,c.title));")?;
        let redirects = connection
            .prepare(
                "SELECT p.title,r.title FROM dump_redirect r JOIN dump_page p ON p.id=r.page_id
        WHERE p.ns=0 AND p.redirect=1 AND r.ns=0 AND COALESCE(r.interwiki,'')='' ",
            )?
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<indexmap::IndexMap<_, _>, _>>()?;
        let mut parents = indexmap::IndexMap::<String, Vec<String>>::new();
        {
            let mut query = connection.prepare("SELECT p.title,COALESCE(t.title,c.title) FROM dump_category c JOIN dump_page p ON p.id=c.source_id
            LEFT JOIN dump_target t ON t.id=c.target_id WHERE p.ns=14 AND (t.ns=14 OR c.title IS NOT NULL)
            AND NOT EXISTS (SELECT 1 FROM dump_page h JOIN dump_prop hp ON hp.page_id=h.id WHERE h.ns=14 AND hp.name='hiddencat' AND h.title=COALESCE(t.title,c.title))")?;
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? {
                parents.entry(row.get(0)?).or_default().push(row.get(1)?);
            }
        }
        connection.execute_batch(
            "CREATE INDEX links_page ON links(id); CREATE INDEX categories_page ON categories(id);",
        )?;
        let start = format!(
            "{}-{}-{}T00:00:00+00:00",
            &release[..4],
            &release[4..6],
            &release[6..]
        );
        let finish = SQL_TABLES
            .iter()
            .filter_map(|table| history_timestamp(manifest["files"][table]["job_updated"].as_str()))
            .max()
            .unwrap_or_else(|| start.clone());
        let manifest_digest =
            DumpClient::<WikimediaHttp>::verified_sha256(manifest_path, None, None)?
                .ok_or("Checksum del manifest mancante")?;
        let metadata = json!({"language":language,"database":database,"source":format!("Wikimedia SQL dumps / {database}"),"sql_release":release,"dump_manifest_sha256":manifest_digest,"complete":true,"demo":false,"started_at":start,"finished_at":finish,"temporal_consistency":"independent_dump_tables_and_publication_interval","creation_date_rule":"unavailable_pending_dump_history_validation","creator_collection_status":"pending","article_features":{"age":"unavailable_pending_dump_history_validation","length":"page.page_len","topic_inputs":"pagelinks + page_props.wikibase_item","topics":"unavailable_no_local_model","quality":"unavailable_no_wikitext_in_sql_dumps","pageviews":"unavailable_not_in_sql_dumps","biographical_gender":"unavailable_not_in_sql_dumps","bot_at_creation":"unavailable_no_complete_historical_membership"},"item_mapping":{"source":"page_props.wikibase_item","complete":true},"dump_manifest":manifest_path.canonicalize()?});
        write_atomic(output, |file| {
            let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file);
            writer.write_all(b"{\"metadata\":")?;
            write_graph_json(
                &GraphRecord {
                    value: &metadata,
                    kind: GraphObject::Metadata,
                },
                &mut writer,
            )?;
            writer.write_all(b",\"pages\":[")?;
            let mut page_query = connection.prepare("SELECT id,data FROM pages ORDER BY id")?;
            let mut link_query =
                connection.prepare("SELECT id,target FROM links ORDER BY id,rowid")?;
            let mut category_query =
                connection.prepare("SELECT id,target FROM categories ORDER BY id,rowid")?;
            let mut pages = page_query.query([])?;
            let mut links = link_query.query([])?;
            let mut categories = category_query.query([])?;
            let mut link = next_relation(&mut links)?;
            let mut category = next_relation(&mut categories)?;
            let mut count = 0;
            while let Some(row) = pages.next()? {
                crate::runtime::check_cancelled()?;
                let id: i64 = row.get(0)?;
                let mut page: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
                let mut targets = Vec::new();
                while link.as_ref().is_some_and(|(source, _)| *source <= id) {
                    if let Some((source, title)) = link.take() {
                        if source == id {
                            targets.push(title);
                        }
                    }
                    link = next_relation(&mut links)?;
                }
                let mut labels = Vec::new();
                while category.as_ref().is_some_and(|(source, _)| *source <= id) {
                    if let Some((source, title)) = category.take() {
                        if source == id {
                            labels.push(title);
                        }
                    }
                    category = next_relation(&mut categories)?;
                }
                page["links"] = json!(targets);
                page["categories"] = json!(labels);
                if count != 0 {
                    writer.write_all(b",")?;
                }
                write_graph_json(
                    &GraphRecord {
                        value: &page,
                        kind: GraphObject::Page,
                    },
                    &mut writer,
                )?;
                count += 1;
            }
            if count != expected {
                return Err("Popolazione del dump cambiata durante l’esportazione".into());
            }
            writer.write_all(b"],\"redirects\":")?;
            write_graph_json(&redirects, &mut writer)?;
            writer.write_all(b",\"category_parents\":")?;
            write_graph_json(&parents, &mut writer)?;
            writer.write_all(b"}")?;
            writer.flush()?;
            Ok(())
        })?;
        println!("Grafo da dump: {expected} voci; {}", output.display());
        Ok(expected)
    })
}

/// Execute the combined CLI stage, rejecting output collisions before either
/// export can replace an input or the other scientific supplement.
#[cfg(feature = "cli")]
pub fn import_dumps(
    manifest_path: &std::path::Path,
    graph_output: &std::path::Path,
    history_output: &std::path::Path,
    work_dir: &std::path::Path,
    options: HistoryOptions,
) -> DumpResult<(usize, usize)> {
    crate::runtime::phase("import.dumps", || {
        crate::runtime::check_cancelled()?;
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(manifest_path)?)?;
        let language = manifest["language"]
            .as_str()
            .ok_or("Lingua del manifest mancante")?;
        let database = crate::wiki_database(language, manifest["database"].as_str())?;
        let release = manifest["sql_release"]
            .as_str()
            .ok_or("Release SQL mancante")?;
        let stage = work_dir.join(format!("{database}-{release}.sqlite"));
        let graph = resolved_path(graph_output)?;
        let history = resolved_path(history_output)?;
        let mut inputs = vec![resolved_path(manifest_path)?, resolved_path(&stage)?];
        if let Some(files) = manifest["files"].as_object() {
            for data in files.values() {
                if let Some(path) = data["path"].as_str() {
                    inputs.push(resolved_path(std::path::Path::new(path))?);
                }
            }
        }
        if graph == history || inputs.contains(&graph) || inputs.contains(&history) {
            return Err("Output e input/staging devono essere distinti".into());
        }
        let graph_count = import_graph(manifest_path, graph_output, work_dir)?;
        let history_count = import_history(manifest_path, history_output, work_dir, options)?;
        Ok((graph_count, history_count))
    })
}

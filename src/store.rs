//! One immutable, relational publication for the complete selected census.
//! JSON is an HTTP encoding and a lossless encoding of scientific attributes,
//! not a second on-disk publication. Reviews live in their independent store.
mod export;
mod publication;
mod query;
mod resources;
use crate::dumps::{self, DumpResult};
pub use publication::publish;
pub use query::PageQuery;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs::File, io::BufReader, path::Path};

pub const RULE: &str = "relational_census_v1";
const PAGE_SIZE: i64 = 25;

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
}

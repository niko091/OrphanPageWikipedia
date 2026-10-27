#[cfg(feature = "cli")]
use super::resolved_path;
use super::DumpResult;

#[cfg(feature = "cli")]
pub(super) fn write_atomic(
    path: &std::path::Path,
    write: impl FnOnce(&mut std::fs::File) -> DumpResult<()>,
) -> DumpResult<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or("Directory dell'output mancante")?;
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .ok_or("Nome dell'output mancante")?
        .to_string_lossy();
    let temporary = parent.join(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        crate::runtime::check_cancelled()?;
        write(&mut file)?;
        file.sync_all()?;
        crate::runtime::check_cancelled()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

#[cfg(feature = "cli")]
pub fn first_metadata(path: &std::path::Path) -> DumpResult<serde_json::Value> {
    struct First<'a>(&'a mut Option<serde_json::Value>);
    impl<'de> serde::de::Visitor<'de> for First<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("metadata as the first JSON section")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
            if map.next_key::<String>()?.as_deref() != Some("metadata") {
                return Err(serde::de::Error::custom("Metadati JSON mancanti"));
            }
            let metadata = map.next_value::<serde_json::Value>()?;
            if !metadata.is_object() {
                return Err(serde::de::Error::custom("Metadati JSON mancanti"));
            }
            *self.0 = Some(metadata);
            // Stop before parsing the potentially multi-gigabyte page array.
            Err(serde::de::Error::custom("metadata complete"))
        }
    }
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let mut metadata = None;
    let result = serde::Deserializer::deserialize_map(&mut deserializer, First(&mut metadata));
    match metadata {
        Some(metadata) => Ok(metadata),
        None => {
            result?;
            Err("Metadati JSON mancanti".into())
        }
    }
}

/// Read the ordered metadata/pages contract, optionally followed by candidate
/// records. Keep one record resident, including records larger than the buffer.
#[cfg(feature = "cli")]
pub(crate) fn visit_ordered_json(
    reader: impl std::io::Read,
    allow_candidates: bool,
    pages_only: bool,
    visit: &mut impl FnMut(&str, Option<&str>, serde_json::Value) -> DumpResult<()>,
) -> DumpResult<()> {
    use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
    use serde_json::Value;
    struct Sections<'a, F>(&'a mut F, bool, bool, &'a mut bool);
    struct Pages<'a, F>(&'a mut F);
    struct Candidates<'a, F>(&'a mut F);
    impl<'de, F> Visitor<'de> for Sections<'_, F>
    where
        F: FnMut(&str, Option<&str>, Value) -> DumpResult<()>,
    {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("ordered metadata, pages and optional link candidates")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
            if map.next_key::<String>()?.as_deref() != Some("metadata") {
                return Err(M::Error::custom("Metadati JSON mancanti"));
            }
            let metadata = map.next_value::<Value>()?;
            if !metadata.is_object() {
                return Err(M::Error::custom("Metadati JSON non validi"));
            }
            (self.0)("metadata", None, metadata).map_err(M::Error::custom)?;
            if map.next_key::<String>()?.as_deref() != Some("pages") {
                return Err(M::Error::custom("Censimento JSON mancante"));
            }
            map.next_value_seed(Pages(self.0))?;
            if self.2 {
                *self.3 = true;
                return Err(M::Error::custom("pages complete"));
            }
            if let Some(key) = map.next_key::<String>()? {
                if !self.1 || key != "link_candidates" {
                    return Err(M::Error::custom("Sezione JSON inattesa"));
                }
                map.next_value_seed(Candidates(self.0))?;
                if map.next_key::<String>()?.is_some() {
                    return Err(M::Error::custom("Sezione JSON inattesa"));
                }
            }
            Ok(())
        }
    }
    impl<'de, F> DeserializeSeed<'de> for Pages<'_, F>
    where
        F: FnMut(&str, Option<&str>, Value) -> DumpResult<()>,
    {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            d.deserialize_seq(self)
        }
    }
    impl<'de, F> Visitor<'de> for Pages<'_, F>
    where
        F: FnMut(&str, Option<&str>, Value) -> DumpResult<()>,
    {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("array of supplement records")
        }
        fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
            while let Some(record) = seq.next_element::<Value>()? {
                if !record.is_object() {
                    return Err(S::Error::custom("Record supplemento non valido"));
                }
                (self.0)("pages", None, record).map_err(S::Error::custom)?;
            }
            Ok(())
        }
    }
    impl<'de, F> DeserializeSeed<'de> for Candidates<'_, F>
    where
        F: FnMut(&str, Option<&str>, Value) -> DumpResult<()>,
    {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            d.deserialize_map(self)
        }
    }
    impl<'de, F> Visitor<'de> for Candidates<'_, F>
    where
        F: FnMut(&str, Option<&str>, Value) -> DumpResult<()>,
    {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("object of candidate records")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
            while let Some(identifier) = map.next_key::<String>()? {
                let record = map.next_value::<Value>()?;
                if !record.is_object() {
                    return Err(M::Error::custom("Record candidate non valido"));
                }
                (self.0)("link_candidates", Some(&identifier), record).map_err(M::Error::custom)?;
            }
            Ok(())
        }
    }
    let mut d = serde_json::Deserializer::from_reader(reader);
    let mut complete = false;
    let result = serde::Deserializer::deserialize_map(
        &mut d,
        Sections(visit, allow_candidates, pages_only, &mut complete),
    );
    if pages_only && complete {
        return Ok(());
    }
    result?;
    d.end()?;
    Ok(())
}

/// The canonical CSV projection, in the same order as the Python exporter.
/// The two candidate fields come from the separate matching records, never
/// from similarly named fields in an observation.
#[cfg(feature = "cli")]
pub const CSV_COLUMNS: &[&str] = &[
    "page_id",
    "title",
    "category",
    "category_status",
    "orphan",
    "in_degree",
    "out_degree",
    "created_at",
    "age_days",
    "history_first_revision_at",
    "history_first_revision_age_days",
    "length_bytes",
    "origin",
    "creator_key",
    "creator_data_status",
    "creator_account_type",
    "creator_tenure_days",
    "creator_prior_edits_main",
    "creator_prior_edits_other",
    "creator_prior_articles",
    "creator_articles_created_total",
    "creator_articles_in_census",
    "creator_orphans_current",
    "creator_orphans_current_share",
    "creator_orphans_at_30_days",
    "history_creator_key",
    "history_creator_status",
    "history_data_status",
    "creator_groups_at_creation_history",
    "creator_continuity_months_before_creation",
    "creator_continuity_bucket_before_creation",
    "creator_prior_edits_main_history",
    "creator_prior_edits_other_history",
    "creator_attribution",
    "creator_registered_at",
    "creator_tenure_status",
    "creator_experience_status",
    "topic_labels",
    "topic_status",
    "topic_input_count",
    "wikibase_item",
    "candidate_count",
    "candidate_status",
    "page_created_at",
    "first_public_revision_at",
    "creation_date_source",
    "creation_date_status",
    "creator_account_type_at_creation",
    "creator_is_bot_now",
    "creator_languages_created",
    "creator_languages_before",
    "creation_tags",
    "import_log_observed",
    "move_log_observed",
    "creator_orphans_at_30_days_status",
    "quality_score",
    "quality_status",
    "content_revision_id",
    "quality_revision_relation",
];

// Python csv.writer calls str() on non-string values: arrays therefore retain
// Python's existing list representation, not JSON. Scientific CSV columns
// contain scalars or arrays; objects are not valid values for these measures.
#[cfg(feature = "cli")]
pub(super) fn csv_value(
    value: &serde_json::Value,
    nonprintable: &regex::Regex,
) -> DumpResult<String> {
    use serde_json::Value;
    fn repr(value: &Value, nonprintable: &regex::Regex, result: &mut String) -> DumpResult<()> {
        use std::fmt::Write;
        match value {
            Value::Null => result.push_str("None"),
            Value::Bool(value) => result.push_str(if *value { "True" } else { "False" }),
            Value::Number(number) => result.push_str(&if number.is_f64() {
                python_float(number)?
            } else {
                number.to_string()
            }),
            Value::String(value) => {
                let quote = if value.contains('\'') && !value.contains('"') {
                    '"'
                } else {
                    '\''
                };
                result.push(quote);
                for c in value.chars() {
                    match c {
                        '\\' => result.push_str("\\\\"),
                        '\t' => result.push_str("\\t"),
                        '\n' => result.push_str("\\n"),
                        '\r' => result.push_str("\\r"),
                        c if c == quote => {
                            result.push('\\');
                            result.push(c);
                        }
                        c if c != ' ' && nonprintable.is_match(c.encode_utf8(&mut [0; 4])) => {
                            let code = u32::from(c);
                            if code <= 0xff {
                                write!(result, "\\x{code:02x}")?;
                            } else if code <= 0xffff {
                                write!(result, "\\u{code:04x}")?;
                            } else {
                                write!(result, "\\U{code:08x}")?;
                            }
                        }
                        _ => result.push(c),
                    }
                }
                result.push(quote);
            }
            Value::Array(values) => {
                result.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        result.push_str(", ");
                    }
                    repr(value, nonprintable, result)?;
                }
                result.push(']');
            }
            Value::Object(_) => return Err("Valore oggetto non valido in una misura CSV".into()),
        }
        Ok(())
    }
    match value {
        Value::Null => Ok(String::new()),
        Value::String(value) => Ok(value.clone()),
        _ => {
            let mut result = String::new();
            repr(value, nonprintable, &mut result)?;
            Ok(result)
        }
    }
}

#[cfg(feature = "cli")]
pub(crate) fn write_csv_row<'a>(
    writer: &mut impl std::io::Write,
    values: impl IntoIterator<Item = &'a serde_json::Value>,
    nonprintable: &regex::Regex,
) -> DumpResult<()> {
    for (position, value) in values.into_iter().enumerate() {
        if position != 0 {
            writer.write_all(b",")?;
        }
        let value = csv_value(value, nonprintable)?;
        if value.contains([',', '"', '\r', '\n']) {
            writer.write_all(b"\"")?;
            let mut parts = value.split('"');
            if let Some(part) = parts.next() {
                writer.write_all(part.as_bytes())?;
            }
            for part in parts {
                writer.write_all(b"\"\"")?;
                writer.write_all(part.as_bytes())?;
            }
            writer.write_all(b"\"")?;
        } else {
            writer.write_all(value.as_bytes())?;
        }
    }
    writer.write_all(b"\r\n")?;
    Ok(())
}

#[cfg(feature = "cli")]
pub(crate) struct CanonicalExport {
    pub(crate) database: rusqlite::Connection,
    pub(crate) metadata: serde_json::Value,
    pub(crate) signature: (u64, i64),
    pub(crate) count: usize,
}

#[cfg(feature = "cli")]
pub(crate) fn stage_export(source: &std::path::Path) -> DumpResult<CanonicalExport> {
    crate::runtime::phase("export.stage", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::params;
        use serde_json::Value;
        use std::io::BufReader;
        let signature = file_signature(source)?;
        let mut database = rusqlite::Connection::open("")?;
        database.execute_batch("PRAGMA cache_size=-8192; PRAGMA temp_store=FILE")?;
        let transaction = database.transaction()?;
        transaction.execute_batch(
        "CREATE TABLE pages(id INTEGER PRIMARY KEY,position INTEGER NOT NULL UNIQUE,orphan INTEGER NOT NULL,data TEXT NOT NULL);
         CREATE TABLE candidates(id INTEGER PRIMARY KEY,status TEXT,count INTEGER,position INTEGER NOT NULL UNIQUE,data TEXT NOT NULL)",
    )?;
        let mut count = 0usize;
        let mut candidate_count = 0i64;
        let mut metadata = None;
        {
            let mut insert_page = transaction.prepare("INSERT INTO pages VALUES(?,?,?,?)")?;
            let mut insert_candidates =
                transaction.prepare("INSERT INTO candidates VALUES(?,?,?,?,?)")?;
            let mut is_orphan = transaction.prepare("SELECT orphan FROM pages WHERE id=?")?;
            visit_ordered_json(
                BufReader::with_capacity(65536, std::fs::File::open(source)?),
                true,
                false,
                &mut |section, identifier, record| {
                    match section {
                        "metadata" => {
                            if record["complete"] != true || record.get("biographies").is_some() {
                                return Err(
                                    "Serve un censimento completo senza arricchimenti".into()
                                );
                            }
                            crate::wiki_database(
                                record["language"].as_str().ok_or("Lingua mancante")?,
                                None,
                            )?;
                            metadata = Some(record);
                        }
                        "pages" => {
                            let id = record["page_id"]
                                .as_i64()
                                .filter(|id| *id > 0)
                                .ok_or("ID osservazione non valido")?;
                            let orphan = record["orphan"]
                                .as_bool()
                                .ok_or("Stato orfana non valido")?;
                            if record.get("biography").is_some()
                                || record.get("gender_group").is_some()
                            {
                                return Err(
                                    "Dataset con arricchimento esterno non autorizzato".into()
                                );
                            }
                            insert_page.execute(params![
                                id,
                                i64::try_from(count)?,
                                orphan,
                                serde_json::to_string(&record)?
                            ])?;
                            count += 1;
                        }
                        "link_candidates" => {
                            let identifier = identifier.ok_or("ID candidate mancante")?;
                            let id = identifier.parse::<i64>()?;
                            if id <= 0
                                || id.to_string() != identifier
                                || !is_orphan.query_row([id], |row| row.get::<_, bool>(0))?
                            {
                                return Err("Candidate non corrispondenti a una voce orfana".into());
                            }
                            let status = match &record["status"] {
                                Value::Null => None,
                                Value::String(value) => Some(value.as_str()),
                                _ => return Err("Stato candidate non valido".into()),
                            };
                            let number = match &record["candidate_count"] {
                                Value::Null => None,
                                value => Some(
                                    value
                                        .as_i64()
                                        .filter(|n| *n >= 0)
                                        .ok_or("Conteggio candidate non valido")?,
                                ),
                            };
                            insert_candidates.execute(params![
                                id,
                                status,
                                number,
                                candidate_count,
                                serde_json::to_string(&record)?
                            ])?;
                            candidate_count += 1;
                        }
                        _ => return Err("Sezione export inattesa".into()),
                    }
                    Ok(())
                },
            )?;
        }
        transaction.commit()?;
        if file_signature(source)? != signature {
            return Err("Esportazione modificata durante la lettura".into());
        }
        Ok(CanonicalExport {
            database,
            metadata: metadata.ok_or("Metadati mancanti")?,
            signature,
            count,
        })
    })
}

/// Prepare the exact CSV projection of a complete ordered canonical export.
/// The caller approves the canonical source; this function does not publish
/// browser transports or a manifest. A private disk-backed SQLite join keeps
/// page order and candidate summaries without retaining a census/evidence map
/// in RAM. Errors preserve the previous CSV and never rewrite the source.
#[cfg(feature = "cli")]
pub fn export_csv(source: &std::path::Path, output: &std::path::Path) -> DumpResult<usize> {
    crate::runtime::phase("export.csv", || {
        crate::runtime::check_cancelled()?;
        use serde_json::{json, Value};
        use std::io::{BufWriter, Write};
        if resolved_path(source)? == resolved_path(output)? {
            return Err("Il CSV deve essere distinto dal dataset originale".into());
        }
        let staged = stage_export(source)?;
        let nonprintable = regex::Regex::new(r"[\p{C}\p{Z}]")?;
        write_atomic(output, |file| {
            let mut writer = BufWriter::with_capacity(65536, file);
            writer.write_all(CSV_COLUMNS.join(",").as_bytes())?;
            writer.write_all(b"\r\n")?;
            let mut query = staged.database.prepare("SELECT p.data,c.status,c.count FROM pages p LEFT JOIN candidates c ON c.id=p.id ORDER BY p.position")?;
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? {
                let page: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
                let status = json!(row.get::<_, Option<String>>(1)?);
                let number = json!(row.get::<_, Option<i64>>(2)?);
                write_csv_row(
                    &mut writer,
                    CSV_COLUMNS.iter().map(|column| match *column {
                        "candidate_count" => &number,
                        "candidate_status" => &status,
                        _ => &page[column],
                    }),
                    &nonprintable,
                )?;
            }
            writer.flush()?;
            if file_signature(source)? != staged.signature {
                return Err("Esportazione modificata durante il CSV".into());
            }
            Ok(())
        })?;
        Ok(staged.count)
    })
}

/// Write the existing sorted, ASCII JSON identity contract in bounded leaves.
/// Objects retain only sorted key references; arrays/evidence are traversed
/// directly. The sink can buffer writes independently of the largest scalar.
#[cfg(feature = "cli")]
pub fn write_canonical_json(
    value: &serde_json::Value,
    writer: &mut impl std::io::Write,
) -> DumpResult<()> {
    write_sorted_json(value, writer, true)
}

#[cfg(feature = "cli")]
pub(super) fn write_sorted_json(
    value: &serde_json::Value,
    writer: &mut impl std::io::Write,
    ascii: bool,
) -> DumpResult<()> {
    use serde_json::Value;
    fn string(value: &str, writer: &mut impl std::io::Write, ascii: bool) -> DumpResult<()> {
        if !ascii {
            serde_json::to_writer(writer, value)?;
            return Ok(());
        }
        for c in serde_json::to_string(value)?.chars() {
            if c.is_ascii() && c != '\u{7f}' {
                writer.write_all(&[c as u8])?;
            } else {
                let mut units = [0; 2];
                for unit in c.encode_utf16(&mut units) {
                    writer.write_all(format!("\\u{unit:04x}").as_bytes())?;
                }
            }
        }
        Ok(())
    }
    match value {
        Value::String(value) => string(value, writer, ascii)?,
        Value::Array(values) => {
            writer.write_all(b"[")?;
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    writer.write_all(b", ")?;
                }
                write_sorted_json(value, writer, ascii)?;
            }
            writer.write_all(b"]")?;
        }
        Value::Object(values) => {
            writer.write_all(b"{")?;
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    writer.write_all(b", ")?;
                }
                string(key, writer, ascii)?;
                writer.write_all(b": ")?;
                write_sorted_json(value, writer, ascii)?;
            }
            writer.write_all(b"}")?;
        }
        Value::Number(number) if number.is_f64() => {
            writer.write_all(python_float(number)?.as_bytes())?
        }
        _ => serde_json::to_writer(writer, value)?,
    }
    Ok(())
}

// serde_json's shortest digits use nearest/even ties, matching Python repr.
// Rust Debug's tie policy differs (e.g. 1255183068570128.25), so do not use it.
pub(crate) fn python_float(number: &serde_json::Number) -> DumpResult<String> {
    let encoded = number.to_string();
    let (sign, encoded) = encoded
        .strip_prefix('-')
        .map_or(("", encoded.as_str()), |s| ("-", s));
    if number.as_f64() == Some(0.0) {
        return Ok(format!("{sign}0.0"));
    }
    let (mantissa, exponent) = encoded.split_once('e').unwrap_or((encoded, "0"));
    let decimal = mantissa.find('.').unwrap_or(mantissa.len());
    let digits = mantissa.replace('.', "");
    let leading = digits.len() - digits.trim_start_matches('0').len();
    let exponent = exponent.parse::<i32>()? + i32::try_from(decimal)? - i32::try_from(leading)? - 1;
    let digits = digits.trim_matches('0');
    if digits.is_empty() {
        return Err("Rappresentazione numerica non valida".into());
    }
    if (-4..16).contains(&exponent) {
        let position = exponent + 1;
        if position <= 0 {
            return Ok(format!(
                "{sign}0.{}{digits}",
                "0".repeat(usize::try_from(-position)?)
            ));
        }
        let position = usize::try_from(position)?;
        if position >= digits.len() {
            return Ok(format!(
                "{sign}{digits}{}.0",
                "0".repeat(position - digits.len())
            ));
        }
        Ok(format!(
            "{sign}{}.{}",
            &digits[..position],
            &digits[position..]
        ))
    } else {
        let fraction = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        Ok(format!(
            "{sign}{}{fraction}e{}{:02}",
            &digits[..1],
            if exponent >= 0 { '+' } else { '-' },
            exponent.unsigned_abs()
        ))
    }
}

#[cfg(feature = "cli")]
#[derive(Default)]
pub(super) struct CanonicalDigest(pub(super) sha2::Sha256);

#[cfg(feature = "cli")]
impl std::io::Write for CanonicalDigest {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Hash the canonical value without serializing the complete document in RAM.
#[cfg(feature = "cli")]
pub fn canonical_json_sha256(value: &serde_json::Value) -> DumpResult<String> {
    use sha2::Digest;
    use std::io::Write;
    let mut digest = CanonicalDigest::default();
    {
        let mut writer = std::io::BufWriter::with_capacity(65536, &mut digest);
        write_canonical_json(value, &mut writer)?;
        writer.flush()?;
    }
    Ok(format!("{:x}", digest.0.finalize()))
}

#[cfg(feature = "cli")]
pub(super) fn python_json(value: &serde_json::Value) -> DumpResult<String> {
    let mut bytes = Vec::new();
    write_canonical_json(value, &mut bytes)?;
    Ok(String::from_utf8(bytes)?)
}

#[cfg(feature = "cli")]
pub(crate) fn file_signature(path: &std::path::Path) -> DumpResult<(u64, i64)> {
    let info = path.metadata()?;
    let time = info
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i128::try_from(duration.as_nanos()))
        .unwrap_or_else(|error| i128::try_from(error.duration().as_nanos()).map(|value| -value))?;
    Ok((info.len(), i64::try_from(time)?))
}

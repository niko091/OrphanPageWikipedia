//! Bounded MySQL dump row decoding. The caller supplies one INSERT statement
//! at a time; this module never retains a table or reads files into memory.

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

pub fn history_activity(
    events: Vec<(String, i32)>,
    creations: Vec<String>,
) -> Result<Vec<(usize, usize, usize, String)>, String> {
    use chrono::{Datelike, NaiveDateTime};
    use std::collections::HashMap;

    fn parse(value: &str) -> Result<NaiveDateTime, String> {
        NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
            .map_err(|_| "invalid MediaWiki History timestamp".to_owned())
    }
    let mut edits = events
        .into_iter()
        .map(|(time, namespace)| parse(&time).map(|parsed| (parsed, namespace)))
        .collect::<Result<Vec<_>, _>>()?;
    edits.sort_unstable_by_key(|row| row.0);
    let mut month_counts = HashMap::<i32, usize>::new();
    for (time, _) in &edits {
        let month = time.year() * 12 + i32::try_from(time.month()).map_err(|e| e.to_string())?;
        *month_counts.entry(month).or_default() += 1;
    }
    // Sort queries once and sweep the edits once. Counting the entire earlier
    // prefix separately for each creation was O(edits * creations) for prolific
    // authors. Keep input order, duplicates and the strict timestamp boundary.
    let mut ordered_creations = creations
        .into_iter()
        .enumerate()
        .map(|(index, value)| parse(&value).map(|creation| (index, creation)))
        .collect::<Result<Vec<_>, _>>()?;
    ordered_creations.sort_unstable_by_key(|(_, creation)| *creation);
    let mut result = (0..ordered_creations.len())
        .map(|_| (0, 0, 0, String::new()))
        .collect::<Vec<_>>();
    let mut earlier = 0;
    let mut main = 0;
    let mut streaks = HashMap::new();
    for (index, creation) in ordered_creations {
        while earlier < edits.len() && edits[earlier].0 < creation {
            main += usize::from(edits[earlier].1 == 0);
            earlier += 1;
        }
        // The creation month is unfinished at the event. Only complete
        // previous calendar months enter the Vital Signs stability streak.
        let month =
            creation.year() * 12 + i32::try_from(creation.month()).map_err(|e| e.to_string())? - 1;
        let active_months = *streaks.entry(month).or_insert_with(|| {
            let mut month = month;
            let mut count = 0;
            while month_counts.get(&month).copied().unwrap_or(0) >= 5 {
                count += 1;
                month -= 1;
            }
            count
        });
        let bucket = match active_months {
            0 => "none",
            1 => "one",
            2 => "two",
            3..=6 => "three_to_six",
            7..=12 => "seven_to_twelve",
            13..=24 => "thirteen_to_twenty_four",
            _ => "over_twenty_four",
        };
        result[index] = (main, earlier - main, active_months, bucket.to_owned());
    }
    Ok(result)
}

pub const DUMP_BASE: &str = "https://dumps.wikimedia.org";
pub const SQL_TABLES: [&str; 7] = [
    "page",
    "pagelinks",
    "linktarget",
    "redirect",
    "page_props",
    "categorylinks",
    "user_groups",
];

/// Small HTTP boundary used by offline fixtures and the official-only client.
#[cfg(feature = "cli")]
pub struct DumpResponse {
    pub status: u16,
    pub headers: std::collections::HashMap<String, String>,
    pub body: Box<dyn std::io::Read>,
}

#[cfg(feature = "cli")]
pub trait DumpHttp {
    fn open(&mut self, url: &str, offset: u64, head: bool) -> Result<DumpResponse, String>;
}

#[cfg(feature = "cli")]
pub struct WikimediaHttp(reqwest::blocking::Client);

#[cfg(feature = "cli")]
impl WikimediaHttp {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .user_agent("OrphanWiki/0.1 (public Wikimedia dump research importer)")
            .timeout(std::time::Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 {
                    attempt.error("Troppi redirect HTTP")
                } else if attempt.url().as_str().starts_with(&format!("{DUMP_BASE}/")) {
                    attempt.follow()
                } else {
                    attempt.error("URL fuori dall’archivio Wikimedia")
                }
            }))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self(client))
    }
}

#[cfg(feature = "cli")]
impl DumpHttp for WikimediaHttp {
    fn open(&mut self, url: &str, offset: u64, head: bool) -> Result<DumpResponse, String> {
        if !url.starts_with(&format!("{DUMP_BASE}/")) {
            return Err("URL fuori dall’archivio Wikimedia".into());
        }
        let mut request = if head {
            self.0.head(url)
        } else {
            self.0.get(url)
        };
        if offset != 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let response = request.send().map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                Ok((
                    name.to_string(),
                    value
                        .to_str()
                        .map_err(|error| error.to_string())?
                        .to_owned(),
                ))
            })
            .collect::<Result<_, String>>()?;
        Ok(DumpResponse {
            status,
            headers,
            body: Box::new(response),
        })
    }
}

/// Sequential, verified acquisition with no writable state in plan mode.
#[cfg(feature = "cli")]
pub struct DumpClient<T: DumpHttp> {
    http: T,
}

#[cfg(feature = "cli")]
impl<T: DumpHttp> DumpClient<T> {
    pub fn new(http: T) -> Self {
        Self { http }
    }

    fn open(&mut self, url: &str, offset: u64, head: bool) -> Result<DumpResponse, String> {
        if !url.starts_with(&format!("{DUMP_BASE}/")) {
            return Err("URL fuori dall’archivio Wikimedia".into());
        }
        self.http.open(url, offset, head)
    }

    fn checked_open(&mut self, url: &str, offset: u64, head: bool) -> Result<DumpResponse, String> {
        let response = self.open(url, offset, head)?;
        if !(200..300).contains(&response.status) {
            return Err(format!("HTTP {}: {url}", response.status));
        }
        Ok(response)
    }

    fn bytes(&mut self, url: &str) -> Result<Vec<u8>, String> {
        let mut response = self.checked_open(url, 0, false)?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut response.body, &mut bytes)
            .map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    fn size(&mut self, url: &str) -> Result<u64, String> {
        let response = self.checked_open(url, 0, true)?;
        let size = response
            .headers
            .get("content-length")
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|&value| value != 0);
        size.ok_or_else(|| format!("Dimensione del dump non valida: {url}"))
    }

    fn hex(value: &str, length: usize) -> bool {
        value.len() == length
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    fn digest(bytes: &[u8]) -> String {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(bytes))
    }

    /// Hash a file once while checking every available publisher checksum.
    pub fn verified_sha256(
        path: &std::path::Path,
        sha1: Option<&str>,
        sha256: Option<&str>,
    ) -> Result<Option<String>, String> {
        use sha2::Digest;
        use std::io::Read;
        let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        let mut one = sha1.map(|_| sha1::Sha1::new());
        let mut two = sha2::Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            two.update(&buffer[..count]);
            if let Some(one) = &mut one {
                one.update(&buffer[..count]);
            }
        }
        let checksum = format!("{:x}", two.finalize());
        if one
            .zip(sha1)
            .is_some_and(|(one, expected)| format!("{:x}", one.finalize()) != expected)
            || sha256.is_some_and(|expected| checksum != expected)
        {
            return Ok(None);
        }
        Ok(Some(checksum))
    }

    /// Publish small operational JSON atomically, preserving identical approved bytes.
    pub fn write_json(path: &std::path::Path, value: &serde_json::Value) -> Result<(), String> {
        use std::io::Write;
        if path.is_file() {
            let old = std::fs::read(path).map_err(|error| error.to_string())?;
            if serde_json::from_slice::<serde_json::Value>(&old)
                .ok()
                .as_ref()
                == Some(value)
            {
                return Ok(());
            }
        }
        write_atomic(path, |file| {
            serde_json::to_writer_pretty(&mut *file, value)?;
            file.write_all(b"\n")?;
            Ok(())
        })
        .map_err(|error| error.to_string())
    }

    fn remove_if_present(path: &std::path::Path) -> Result<(), String> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Resume only ranges tied to this immutable URL, size and available checksum.
    pub fn fetch(
        &mut self,
        url: &str,
        path: &std::path::Path,
        expected_size: u64,
        sha1: Option<&str>,
        sha256: Option<&str>,
    ) -> Result<String, String> {
        use serde_json::json;
        use std::io::{Read, Write};
        if expected_size == 0
            || sha1.is_some_and(|value| !Self::hex(value, 40))
            || sha256.is_some_and(|value| !Self::hex(value, 64))
        {
            return Err("Dimensione o checksum atteso non valido".into());
        }
        if !url.starts_with(&format!("{DUMP_BASE}/")) {
            return Err("URL fuori dall’archivio Wikimedia".into());
        }
        std::fs::create_dir_all(path.parent().ok_or("Directory del dump mancante")?)
            .map_err(|error| error.to_string())?;
        if path.is_file()
            && path.metadata().map_err(|error| error.to_string())?.len() == expected_size
        {
            if let Some(checksum) = Self::verified_sha256(path, sha1, sha256)? {
                return Ok(checksum);
            }
        }
        let name = path
            .file_name()
            .ok_or("Nome dump non valido")?
            .to_string_lossy();
        let partial = path.with_file_name(format!("{name}.part"));
        let source = path.with_file_name(format!("{name}.part-source.json"));
        let mut identity = json!({"url":url,"bytes":expected_size});
        if let Some(value) = sha1 {
            identity["sha1"] = json!(value);
        }
        if let Some(value) = sha256 {
            identity["sha256"] = json!(value);
        }
        let previous = if source.is_file() {
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(&source).map_err(|error| error.to_string())?,
            )
            .ok()
        } else {
            None
        };
        if previous.as_ref() != Some(&identity) {
            Self::remove_if_present(&partial)?;
            Self::write_json(&source, &identity)?;
        }
        let mut offset = match partial.metadata() {
            Ok(info) => info.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.to_string()),
        };
        if offset == expected_size {
            if let Some(checksum) = Self::verified_sha256(&partial, sha1, sha256)? {
                std::fs::rename(&partial, path).map_err(|error| error.to_string())?;
                Self::remove_if_present(&source)?;
                return Ok(checksum);
            }
        }
        if offset >= expected_size {
            Self::remove_if_present(&partial)?;
            offset = 0;
        }
        let mut response = self.open(url, offset, false)?;
        if response.status == 416 {
            Self::remove_if_present(&partial)?;
            offset = 0;
            response = self.open(url, 0, false)?;
        }
        if !(200..300).contains(&response.status) {
            return Err(format!("HTTP {}: {url}", response.status));
        }
        let resumed = offset != 0 && response.status == 206;
        if resumed
            && response.headers.get("content-range")
                != Some(&format!(
                    "bytes {offset}-{}/{}",
                    expected_size - 1,
                    expected_size
                ))
        {
            return Err(format!("Risposta Range inattesa: {name}"));
        }
        if !resumed && response.status == 206 {
            return Err(format!("Risposta parziale inattesa: {name}"));
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&partial)
            .map_err(|error| error.to_string())?;
        let mut received = if resumed { offset } else { 0 };
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let count = response
                .body
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            received = received
                .checked_add(count as u64)
                .ok_or("Dimensione download fuori intervallo")?;
            if received > expected_size {
                return Err(format!("Download oltre la dimensione attesa: {name}"));
            }
            file.write_all(&buffer[..count])
                .map_err(|error| error.to_string())?;
        }
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        if received != expected_size {
            return Err(format!("Download incompleto: {name}"));
        }
        let checksum = match Self::verified_sha256(&partial, sha1, sha256)? {
            Some(value) => value,
            None => {
                Self::remove_if_present(&partial)?;
                return Err(format!("Checksum Wikimedia errato: {name}"));
            }
        };
        std::fs::rename(&partial, path).map_err(|error| error.to_string())?;
        Self::remove_if_present(&source)?;
        Ok(checksum)
    }

    pub fn history_partitions(
        names: &[String],
        release: &str,
        database: &str,
    ) -> Result<Vec<String>, String> {
        use std::collections::BTreeSet;
        let pattern = regex::Regex::new(&format!(
            r"^{}\.{}\.(all-time|[0-9]{{4}}(?:-[0-9]{{2}})?)\.tsv\.bz2$",
            regex::escape(release),
            regex::escape(database)
        ))
        .map_err(|error| error.to_string())?;
        let selected = names
            .iter()
            .filter(|name| pattern.is_match(name))
            .cloned()
            .collect::<BTreeSet<_>>();
        let periods = selected
            .iter()
            .filter_map(|name| {
                pattern
                    .captures(name)
                    .and_then(|capture| capture.get(1))
                    .map(|period| period.as_str().to_owned())
            })
            .collect::<Vec<_>>();
        if periods.is_empty() {
            return Err(format!("Nessuna partizione MediaWiki History: {database}"));
        }
        if periods.iter().any(|value| value == "all-time") {
            if periods.len() != 1 {
                return Err(format!(
                    "Partizioni MediaWiki History sovrapposte: {database}"
                ));
            }
            return Ok(selected.into_iter().collect());
        }
        let mut covered = BTreeSet::new();
        for period in periods {
            let year = period[..4]
                .parse::<u32>()
                .map_err(|error| error.to_string())?;
            let months = if period.len() == 4 {
                1..=12
            } else {
                let month = period[5..]
                    .parse::<u32>()
                    .map_err(|error| error.to_string())?;
                if !(1..=12).contains(&month) {
                    return Err(format!("Mese MediaWiki History non valido: {period}"));
                }
                month..=month
            };
            for month in months {
                if !covered.insert(year * 12 + month) {
                    return Err(format!(
                        "Partizioni MediaWiki History sovrapposte: {database}"
                    ));
                }
            }
        }
        if covered
            .first()
            .zip(covered.last())
            .is_none_or(|(first, last)| covered.len() as u32 != last - first + 1)
        {
            return Err(format!("Partizioni MediaWiki History mancanti: {database}"));
        }
        Ok(selected.into_iter().collect())
    }

    /// Read anchor attributes, honoring quoted `>` and ignoring unrelated tags.
    fn names(html: &str) -> Result<Vec<String>, String> {
        let tags = regex::Regex::new(r#"(?is)<a\s+(?:[^>"']|"[^"]*"|'[^']*')*>"#)
            .map_err(|error| error.to_string())?;
        let attributes =
            regex::Regex::new(r#"(?is)([^\s=<>/]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?"#)
                .map_err(|error| error.to_string())?;
        let mut names = Vec::new();
        for tag in tags.find_iter(html) {
            let tag = tag.as_str();
            for attribute in attributes.captures_iter(&tag[2..tag.len() - 1]) {
                if attribute
                    .get(1)
                    .is_some_and(|value| value.as_str().eq_ignore_ascii_case("href"))
                {
                    if let Some(value) = attribute
                        .get(2)
                        .or_else(|| attribute.get(3))
                        .or_else(|| attribute.get(4))
                    {
                        let value = value.as_str();
                        // Official filenames are ASCII and contain no HTML entities.
                        // Reject encoded names instead of accepting an incomplete census.
                        if value.contains('&') && (value.contains("tsv") || value.contains("bz2")) {
                            return Err("Nome History codificato non supportato".into());
                        }
                        names.push(value.to_owned());
                    }
                }
            }
        }
        Ok(names)
    }

    pub fn verify_bzip2(path: &std::path::Path) -> Result<(), String> {
        let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        let mut stream = bzip2::read::MultiBzDecoder::new(file);
        std::io::copy(&mut stream, &mut std::io::sink()).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn fetch_dumps(
        &mut self,
        editions: &[(String, String)],
        sql_release: &str,
        history_release: &str,
        root: &std::path::Path,
        plan: bool,
    ) -> Result<Vec<serde_json::Value>, String> {
        use serde_json::{json, Map, Value};
        let sql_pattern = regex::Regex::new(r"^20[0-9]{6}$").map_err(|error| error.to_string())?;
        let history_pattern = regex::Regex::new(r"^20[0-9]{2}-(0[1-9]|1[0-2])$")
            .map_err(|error| error.to_string())?;
        if !sql_pattern.is_match(sql_release) {
            return Err("Data SQL richiesta in formato YYYYMMDD".into());
        }
        if !history_pattern.is_match(history_release) {
            return Err("Release MediaWiki History richiesta in formato YYYY-MM".into());
        }
        Self::validate_editions(editions)?;
        let mut results = Vec::new();
        for (language, database) in editions {
            let status_url = format!("{DUMP_BASE}/{database}/{sql_release}/dumpstatus.json");
            let status: Value = serde_json::from_slice(&self.bytes(&status_url)?)
                .map_err(|error| error.to_string())?;
            let mut files = Map::new();
            let mut updates = Map::new();
            if let Some(jobs) = status.get("jobs").and_then(Value::as_object) {
                for job in jobs
                    .values()
                    .filter(|job| job.get("status").and_then(Value::as_str) == Some("done"))
                {
                    if let Some(job_files) = job.get("files").and_then(Value::as_object) {
                        for (name, detail) in job_files {
                            if files.get(name).is_some_and(|previous| previous != detail) {
                                return Err(format!("Identità SQL conflittuale: {name}"));
                            }
                            files.insert(name.clone(), detail.clone());
                            updates.insert(
                                name.clone(),
                                job.get("updated").cloned().unwrap_or(Value::Null),
                            );
                        }
                    }
                }
            }
            let mut selected = Vec::new();
            let mut total = 0u64;
            for table in SQL_TABLES {
                let name = format!("{database}-{sql_release}-{table}.sql.gz");
                let detail = files
                    .get(&name)
                    .and_then(Value::as_object)
                    .ok_or_else(|| format!("Dump SQL incompleto o senza SHA-1: {name}"))?;
                let sha1 = detail
                    .get("sha1")
                    .and_then(Value::as_str)
                    .filter(|value| Self::hex(value, 40))
                    .ok_or_else(|| format!("Dump SQL incompleto o senza SHA-1: {name}"))?;
                let url = format!("/{database}/{sql_release}/{name}");
                if detail.get("url").and_then(Value::as_str) != Some(&url) {
                    return Err(format!("Percorso del dump SQL inatteso: {name}"));
                }
                let size = detail
                    .get("size")
                    .and_then(Value::as_u64)
                    .filter(|&size| size != 0)
                    .ok_or_else(|| format!("Dimensione del dump non valida: {name}"))?;
                total = total
                    .checked_add(size)
                    .ok_or("Dimensione dei dump fuori intervallo")?;
                selected.push((
                    table,
                    name,
                    format!("{DUMP_BASE}{url}"),
                    size,
                    sha1.to_owned(),
                ));
            }
            let history_url =
                format!("{DUMP_BASE}/other/mediawiki_history/{history_release}/{database}/");
            let html =
                String::from_utf8(self.bytes(&history_url)?).map_err(|error| error.to_string())?;
            let history_names =
                Self::history_partitions(&Self::names(&html)?, history_release, database)?;
            let mut histories = Vec::new();
            for name in &history_names {
                let url = format!("{history_url}{name}");
                let size = self.size(&url)?;
                total = total
                    .checked_add(size)
                    .ok_or("Dimensione dei dump fuori intervallo")?;
                histories.push((name, url, size));
            }
            println!(
                "{language}: {} SQL, {} partizioni History, {:.2} GiB compressi",
                selected.len(),
                history_names.len(),
                total as f64 / 1024f64.powi(3)
            );
            if plan {
                results.push(json!({"language":language,"database":database,"sql_release":sql_release,"history_release":history_release,"compressed_bytes":total,"history_partitions":history_names.len()}));
                continue;
            }
            let mut records = Map::new();
            for (table, name, url, size, sha1) in selected {
                let path = root.join(database).join(sql_release).join(&name);
                let checksum = self.fetch(&url, &path, size, Some(&sha1), None)?;
                records.insert(table.into(), json!({"path":path.canonicalize().map_err(|error| error.to_string())?,"bytes":size,"sha1_publisher":sha1,"sha256_local":checksum,"job_updated":updates.get(&name).unwrap_or(&Value::Null)}));
                println!("Dump {database}: {name} verificato");
            }
            for (name, url, size) in histories {
                let path = root.join(database).join(history_release).join(name);
                let mut checksum = self.fetch(&url, &path, size, None, None)?;
                if Self::verify_bzip2(&path).is_err() {
                    Self::remove_if_present(&path)?;
                    checksum = self.fetch(&url, &path, size, None, None)?;
                    Self::verify_bzip2(&path)?;
                }
                records.insert(name.clone(), json!({"path":path.canonicalize().map_err(|error| error.to_string())?,"bytes":size,"sha256_local":checksum,"checksum_scope":"local_only"}));
                println!("Dump {database}: {name} verificato");
            }
            let manifest = json!({"language":language,"database":database,"sql_release":sql_release,"history_release":history_release,"sql_status_url":status_url,"history_index_url":history_url,"files":records});
            let destination = root
                .join(database)
                .join(format!("manifest-{sql_release}-{history_release}.json"));
            Self::write_json(&destination, &manifest)?;
            println!("Manifest verificato: {}", destination.display());
            results.push(manifest);
        }
        Ok(results)
    }

    fn validate_editions(editions: &[(String, String)]) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        if editions.is_empty() {
            return Err("Specificare lingue distinte".into());
        }
        for (language, database) in editions {
            crate::wiki_database(language, Some(database))?;
            if !seen.insert(language) {
                return Err("Specificare lingue distinte".into());
            }
        }
        Ok(())
    }

    pub fn fetch_content(
        &mut self,
        editions: &[(String, String)],
        release: &str,
        root: &std::path::Path,
        plan: bool,
    ) -> Result<Vec<serde_json::Value>, String> {
        use serde_json::{json, Map};
        let pattern = regex::Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$")
            .map_err(|error| error.to_string())?;
        if !pattern.is_match(release)
            || chrono::NaiveDate::parse_from_str(release, "%Y-%m-%d").is_err()
        {
            return Err("Release contenuto richiesta: YYYY-MM-DD".into());
        }
        Self::validate_editions(editions)?;
        let line_pattern = regex::Regex::new(r"^([a-f0-9]{64})\s+\*?(?:\./)?([\w.-]+\.xml\.bz2)$")
            .map_err(|error| error.to_string())?;
        let mut results = Vec::new();
        for (language, database) in editions {
            let base = format!(
                "{DUMP_BASE}/other/mediawiki_content_current/{database}/{release}/xml/bzip2/"
            );
            let publisher = self.bytes(&format!("{base}SHA256SUMS"))?;
            let text = std::str::from_utf8(&publisher).map_err(|error| error.to_string())?;
            let mut files = Map::new();
            let mut selected = Vec::new();
            let mut total = 0u64;
            for line in text.lines() {
                let capture = line_pattern
                    .captures(line)
                    .ok_or("SHA256SUMS contenuto non valido")?;
                let digest = capture.get(1).ok_or("Checksum XML mancante")?.as_str();
                let name = capture.get(2).ok_or("Nome XML mancante")?.as_str();
                if !name.starts_with(&format!("{database}-{release}-")) || files.contains_key(name)
                {
                    return Err("Identità o nome duplicato nel contenuto XML".into());
                }
                let url = format!("{base}{name}");
                let size = self.size(&url)?;
                total = total
                    .checked_add(size)
                    .ok_or("Dimensione dei dump fuori intervallo")?;
                let path = root.join(database).join("content").join(release).join(name);
                files.insert(name.into(), json!({"url":url,"path":path,"bytes":size,"sha256_publisher":digest,"sha256_local":digest}));
                selected.push((url, path, size, digest.to_owned()));
            }
            if files.is_empty() {
                return Err("Nessun file di contenuto nel manifest ufficiale".into());
            }
            let manifest = json!({"language":language,"database":database,"content_release":release,"source":"mediawiki_content_current","complete":true,"publisher_sha256":Self::digest(&publisher),"files":files});
            println!(
                "Contenuto {language}: {} partizioni, {total} byte",
                selected.len()
            );
            if !plan {
                for (url, path, size, digest) in selected {
                    self.fetch(&url, &path, size, None, Some(&digest))?;
                }
                Self::write_json(
                    &root.join(database).join(format!("content-{release}.json")),
                    &manifest,
                )?;
            }
            results.push(manifest);
        }
        Ok(results)
    }
}

#[cfg(feature = "cli")]
type DumpResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[cfg(feature = "cli")]
fn write_atomic(
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
        write(&mut file)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

#[cfg(feature = "cli")]
fn read_sql(
    path: &std::path::Path,
    table: &str,
    mut consume: impl FnMut(&[String], Vec<Vec<Option<String>>>) -> DumpResult<()>,
) -> DumpResult<()> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let mut stream = std::io::BufReader::new(flate2::read::MultiGzDecoder::new(file));
    let header = format!("INSERT INTO `{table}` VALUES");
    let mut columns = Vec::new();
    let mut statement = String::new();
    let mut inside = false;
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        if stream.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&bytes);
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
            consume(&columns, parse_insert(&statement)?)?;
            statement.clear();
            inside = false;
        }
    }
    if inside {
        return Err(format!("INSERT SQL troncato: {table}").into());
    }
    Ok(())
}

#[cfg(feature = "cli")]
fn load_sql_table(
    connection: &rusqlite::Connection,
    table: &str,
    path: &std::path::Path,
    digest: &str,
) -> DumpResult<()> {
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
}

#[cfg(feature = "cli")]
fn resolved_path(path: &std::path::Path) -> DumpResult<std::path::PathBuf> {
    use std::path::Component;
    let path = std::env::current_dir()?.join(path);
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(component.as_os_str()),
        }
        if normalized.exists() {
            normalized = normalized.canonicalize()?;
        }
    }
    Ok(normalized)
}

#[cfg(feature = "cli")]
fn history_timestamp(value: Option<&str>) -> Option<String> {
    use chrono::Timelike;
    let value = value?;
    let pattern =
        regex::Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?$")
            .ok()?;
    if !pattern.is_match(value) {
        return None;
    }
    let parsed = chrono::NaiveDateTime::parse_from_str(&value[..19], "%Y-%m-%d %H:%M:%S").ok()?;
    if value.starts_with("0000") || parsed.nanosecond() >= 1_000_000_000 {
        return None;
    }
    let mut result = parsed.format("%Y-%m-%dT%H:%M:%S").to_string();
    let fraction = value.get(20..).unwrap_or("");
    let micros = format!("{fraction:0<6}")[..6].parse::<u32>().ok()?;
    if micros != 0 {
        result.push_str(&format!(".{micros:06}"));
    }
    result.push_str("+00:00");
    Some(result)
}

#[cfg(feature = "cli")]
fn next_relation(rows: &mut rusqlite::Rows<'_>) -> DumpResult<Option<(i64, String)>> {
    rows.next()?
        .map(|row| Ok((row.get(0)?, row.get(1)?)))
        .transpose()
}

/// Import verified SQL archives using the existing table and checkpoint contracts.
/// The caller owns separate output/staging paths; collected inputs are read only.
#[cfg(feature = "cli")]
pub fn import_graph(
    manifest_path: &std::path::Path,
    output: &std::path::Path,
    work_dir: &std::path::Path,
) -> DumpResult<usize> {
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
    if resolved_output == resolved_path(manifest_path)? || resolved_output == resolved_path(&stage)?
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
        let path = std::path::Path::new(record["path"].as_str().ok_or("Percorso SQL mancante")?);
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
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
    let mut parents = std::collections::BTreeMap::<String, Vec<String>>::new();
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
    let manifest_digest = DumpClient::<WikimediaHttp>::verified_sha256(manifest_path, None, None)?
        .ok_or("Checksum del manifest mancante")?;
    let metadata = json!({"language":language,"database":database,"source":format!("Wikimedia SQL dumps / {database}"),"sql_release":release,"dump_manifest_sha256":manifest_digest,"complete":true,"demo":false,"started_at":start,"finished_at":finish,"temporal_consistency":"independent_dump_tables_and_publication_interval","creation_date_rule":"unavailable_pending_dump_history_validation","creator_collection_status":"pending","article_features":{"age":"unavailable_pending_dump_history_validation","length":"page.page_len","topic_inputs":"pagelinks + page_props.wikibase_item","topics":"unavailable_no_local_model","quality":"unavailable_no_wikitext_in_sql_dumps","pageviews":"unavailable_not_in_sql_dumps","biographical_gender":"unavailable_not_in_sql_dumps","bot_at_creation":"unavailable_no_complete_historical_membership"},"item_mapping":{"source":"page_props.wikibase_item","complete":true},"dump_manifest":manifest_path.canonicalize()?});
    write_atomic(output, |file| {
        let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file);
        writer.write_all(b"{\"metadata\":")?;
        serde_json::to_writer(&mut writer, &metadata)?;
        writer.write_all(b",\"pages\":[")?;
        let mut page_query = connection.prepare("SELECT id,data FROM pages ORDER BY id")?;
        let mut link_query = connection.prepare("SELECT id,target FROM links ORDER BY id,rowid")?;
        let mut category_query =
            connection.prepare("SELECT id,target FROM categories ORDER BY id,rowid")?;
        let mut pages = page_query.query([])?;
        let mut links = link_query.query([])?;
        let mut categories = category_query.query([])?;
        let mut link = next_relation(&mut links)?;
        let mut category = next_relation(&mut categories)?;
        let mut count = 0;
        while let Some(row) = pages.next()? {
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
            serde_json::to_writer(&mut writer, &page)?;
            count += 1;
        }
        if count != expected {
            return Err("Popolazione del dump cambiata durante l’esportazione".into());
        }
        writer.write_all(b"],\"redirects\":")?;
        serde_json::to_writer(&mut writer, &redirects)?;
        writer.write_all(b",\"category_parents\":")?;
        serde_json::to_writer(&mut writer, &parents)?;
        writer.write_all(b"}")?;
        writer.flush()?;
        Ok(())
    })?;
    println!("Grafo da dump: {expected} voci; {}", output.display());
    Ok(expected)
}

#[cfg(test)]
mod tests {
    use super::parse_insert;

    use super::history_activity;

    #[cfg(feature = "cli")]
    #[test]
    fn job_timestamps_preserve_python_microseconds_and_reject_invalid_dates() {
        for (value, expected) in [
            ("2026-10-02 01:02:03", "2026-10-02T01:02:03+00:00"),
            ("2026-10-02 01:02:03.000", "2026-10-02T01:02:03+00:00"),
            ("2026-10-02 01:02:03.12", "2026-10-02T01:02:03.120000+00:00"),
            (
                "2026-10-02 01:02:03.123456789",
                "2026-10-02T01:02:03.123456+00:00",
            ),
        ] {
            assert_eq!(
                super::history_timestamp(Some(value)).as_deref(),
                Some(expected)
            );
        }
        for value in [
            "invalid",
            "0000-01-01 00:00:00",
            "2026-02-30 00:00:00",
            "2026-01-01 00:00:60",
        ] {
            assert_eq!(super::history_timestamp(Some(value)), None);
        }
        assert_eq!(super::history_timestamp(None), None);
    }

    #[test]
    fn prior_activity_uses_strict_time_and_completed_months() {
        let mut events = Vec::new();
        for day in 1..=5 {
            events.push((format!("2026-01-{day:02} 10:00:00.0"), 0));
            events.push((format!("2026-02-{day:02} 10:00:00.0"), 1));
        }
        events.push(("2026-03-01 12:00:00.0".to_owned(), 0));
        let metrics = history_activity(events, vec!["2026-03-01 12:00:00.0".to_owned()]).unwrap();
        assert_eq!(metrics, vec![(5, 5, 2, "two".to_owned())]);
    }

    #[test]
    fn prior_activity_sweep_preserves_query_order_duplicates_and_time_boundaries() {
        let events = vec![
            ("2026-03-01 12:00:00.0".to_owned(), 0),
            ("2026-01-01 00:00:00.0".to_owned(), -1),
            ("2026-02-01 00:00:00.0".to_owned(), 0),
            ("2026-02-01 00:00:00.0".to_owned(), 1),
        ];
        let queries = [
            "2026-04-01 00:00:00.0",
            "2026-02-01 00:00:00.0",
            "2026-01-01 00:00:00.0",
            "2026-04-01 00:00:00.0",
            "2025-01-01 00:00:00.0",
            "2026-02-01 00:00:00.1",
        ];
        let result =
            history_activity(events, queries.iter().map(|s| (*s).to_owned()).collect()).unwrap();
        let expected = [(2, 2), (0, 1), (0, 0), (2, 2), (0, 0), (1, 2)];
        for (actual, (main, other)) in result.iter().zip(expected) {
            assert_eq!(actual, &(main, other, 0, "none".to_owned()));
        }
        assert_eq!(result.len(), queries.len());
        assert!(history_activity(Vec::new(), vec!["invalid".to_owned()]).is_err());
        assert!(history_activity(vec![("invalid".to_owned(), 0)], Vec::new()).is_err());
        assert_eq!(
            history_activity(Vec::new(), Vec::new()).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn prior_activity_caches_streaks_without_changing_creation_month_exclusion() {
        let mut events = Vec::new();
        for month in 1..=4 {
            for day in 1..=5 {
                events.push((format!("2026-{month:02}-{day:02} 10:00:00.0"), 0));
            }
        }
        let queries = [
            "2026-04-30 12:00:00.0",
            "2026-02-01 12:00:00.0",
            "2026-04-01 12:00:00.0",
            "2026-05-01 12:00:00.0",
        ];
        let result =
            history_activity(events, queries.iter().map(|s| (*s).to_owned()).collect()).unwrap();
        assert_eq!(
            result,
            vec![
                (20, 0, 3, "three_to_six".to_owned()),
                (6, 0, 1, "one".to_owned()),
                (16, 0, 3, "three_to_six".to_owned()),
                (20, 0, 4, "three_to_six".to_owned()),
            ]
        );
    }

    #[test]
    fn parses_mysql_values_and_escapes() {
        let sql = "INSERT INTO `page` VALUES (1,'A\\'B',NULL,-2),(2,'Vèneto\\n',3,4);\n";
        assert_eq!(
            parse_insert(sql).unwrap(),
            vec![
                vec![
                    Some("1".into()),
                    Some("A'B".into()),
                    None,
                    Some("-2".into())
                ],
                vec![
                    Some("2".into()),
                    Some("Vèneto\n".into()),
                    Some("3".into()),
                    Some("4".into())
                ]
            ]
        );
    }

    #[test]
    fn parses_multiline_dump_tuple() {
        assert_eq!(
            parse_insert("(5,0,'Łéngua_vèneta',0,NULL),\n").unwrap(),
            vec![vec![
                Some("5".into()),
                Some("0".into()),
                Some("Łéngua_vèneta".into()),
                Some("0".into()),
                None
            ]]
        );
    }

    #[test]
    fn rejects_truncated_or_malformed_input() {
        for sql in [
            "INSERT INTO `page` VALUES (1,'broken",
            "INSERT INTO `page` VALUES (1,)",
        ] {
            assert!(parse_insert(sql).is_err(), "{sql}");
        }
    }
}

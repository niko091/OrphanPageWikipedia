//! Bounded MySQL decoding and optional standalone verified dump acquisition,
//! SQL graph import, MediaWiki History reconstruction and XML structural quality.

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
            serde_json::from_str(include_str!("../config/quality.json"))
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
    static PATTERN: std::sync::OnceLock<Result<regex::Regex, regex::Error>> =
        std::sync::OnceLock::new();
    let pattern = PATTERN
        .get_or_init(|| {
            regex::Regex::new(
                r"^[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?$",
            )
        })
        .as_ref()
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

#[cfg(feature = "cli")]
type ContentNamespaces = std::collections::HashMap<i32, String>;

#[cfg(feature = "cli")]
type ContentConsumer<'a> = dyn FnMut(ContentPage, &ContentNamespaces) -> DumpResult<()> + 'a;

#[cfg(feature = "cli")]
#[derive(Default)]
struct ContentElement {
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
struct ContentPage {
    id: i64,
    revision: i64,
    namespace: i64,
    redirect: bool,
    text: Option<String>,
}

/// Parse at most one page tree at a time. Text never leaves transient memory.
#[cfg(feature = "cli")]
fn scan_content(
    path: &std::path::Path,
    database: &str,
    mut consume: impl FnMut(ContentPage, &std::collections::HashMap<i32, String>) -> DumpResult<()>,
) -> DumpResult<()> {
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
                .filter(|child| child.name == "revision" && child.namespace == element.namespace)
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
            Event::DocType(_) => return Err("DTD non previsto nell'export XML ufficiale".into()),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if depth != 0 || !identified {
        return Err("Identità XML non verificata o documento incompleto".into());
    }
    Ok(())
}

#[cfg(feature = "cli")]
fn check_xml_characters(text: &str) -> DumpResult<()> {
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
fn first_metadata(path: &std::path::Path) -> DumpResult<serde_json::Value> {
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

// Existing quality checkpoint identities use Python's sorted ASCII JSON with
// default separators. Numeric model constants retain their frozen spellings.
#[cfg(feature = "cli")]
fn python_json(value: &serde_json::Value) -> DumpResult<String> {
    use serde_json::Value;
    Ok(match value {
        Value::String(_) => {
            let mut output = String::new();
            for c in serde_json::to_string(value)?.chars() {
                if c.is_ascii() && c != '\u{7f}' {
                    output.push(c);
                } else {
                    let mut units = [0; 2];
                    for unit in c.encode_utf16(&mut units) {
                        output.push_str(&format!("\\u{unit:04x}"));
                    }
                }
            }
            output
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_json)
                .collect::<DumpResult<Vec<_>>>()?
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| Ok(format!(
                    "{}: {}",
                    python_json(&Value::String(key.clone()))?,
                    python_json(value)?
                )))
                .collect::<DumpResult<Vec<_>>>()?
                .join(", ")
        ),
        _ => serde_json::to_string(value)?,
    })
}

#[cfg(feature = "cli")]
fn file_signature(path: &std::path::Path) -> DumpResult<(u64, i64)> {
    let info = path.metadata()?;
    let time = info
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i128::try_from(duration.as_nanos()))
        .unwrap_or_else(|error| i128::try_from(error.duration().as_nanos()).map(|value| -value))?;
    Ok((info.len(), i64::try_from(time)?))
}

#[cfg(feature = "cli")]
#[derive(serde::Deserialize)]
struct ContentManifest {
    language: String,
    database: String,
    source: String,
    complete: bool,
    content_release: String,
    #[serde(deserialize_with = "history_files")]
    files: Vec<(String, serde_json::Value)>,
}

#[cfg(feature = "cli")]
fn flush_quality(
    connection: &rusqlite::Connection,
    batch: &mut Vec<(ContentPage, std::collections::HashMap<i32, String>)>,
    language: &str,
    spec: &serde_json::Value,
    name: &str,
    extractor: &mut Option<(std::collections::HashMap<i32, String>, QualityExtractor)>,
) -> DumpResult<()> {
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
        let path = std::path::Path::new(record["path"].as_str().ok_or("Percorso XML mancante")?);
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
        let path = std::path::Path::new(record["path"].as_str().ok_or("Percorso XML mancante")?);
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
    let mut maxima = [0.0; 6];
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
            percentile =
                first + (rank - offset as f64) * (pair.last().copied().unwrap_or(first) - first);
        }
        *maximum = minimum.max(percentile);
    }
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
        "same_revision_pages":same,"different_revision_pages":different,"unavailable_revision_pages":unavailable,"normalizers":maxima,"content_files":content_files});
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
}

/// Operational History controls do not change scientific checkpoint identities.
#[cfg(feature = "cli")]
#[derive(Clone, Copy)]
pub struct HistoryOptions {
    pub cache_mib: u32,
    pub commit_rows: usize,
    pub creation_cache: usize,
}

#[cfg(feature = "cli")]
impl Default for HistoryOptions {
    fn default() -> Self {
        Self {
            cache_mib: 256,
            commit_rows: 250_000,
            creation_cache: 100_000,
        }
    }
}

#[cfg(feature = "cli")]
const HISTORY_RULE: &str = "mediawiki_history_page_creation_v2";

// Preserve partition order without requiring an ordered JSON-map dependency.
#[cfg(feature = "cli")]
#[derive(serde::Deserialize)]
struct HistoryManifest {
    language: String,
    database: Option<String>,
    sql_release: String,
    history_release: String,
    #[serde(deserialize_with = "history_files")]
    files: Vec<(String, serde_json::Value)>,
}

#[cfg(feature = "cli")]
fn history_files<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<(String, serde_json::Value)>, D::Error> {
    struct Files;
    impl<'de> serde::de::Visitor<'de> for Files {
        type Value = Vec<(String, serde_json::Value)>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a dump file map")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut files = Vec::new();
            while let Some(entry) = map.next_entry()? {
                files.push(entry);
            }
            Ok(files)
        }
    }
    deserializer.deserialize_map(Files)
}

#[cfg(feature = "cli")]
struct HistoryScan<'a> {
    database: &'a str,
    pages: &'a std::collections::BTreeSet<i64>,
    users: Option<&'a std::collections::BTreeSet<i64>>,
    options: HistoryOptions,
}

#[cfg(feature = "cli")]
#[derive(Default)]
struct HistoryBuffer {
    rows: Vec<Vec<rusqlite::types::Value>>,
    events: Vec<Vec<rusqlite::types::Value>>,
    creations: std::collections::BTreeSet<(i64, String)>,
    scanned: usize,
    retained: usize,
    pending: usize,
    commits: usize,
    cache_hits: usize,
    creation_rows: usize,
    scratch_rows: usize,
    scratch_bytes: u64,
    max_buffer: usize,
    sqlite_seconds: f64,
}

#[cfg(feature = "cli")]
impl HistoryBuffer {
    fn flush(
        &mut self,
        connection: &rusqlite::Connection,
        name: &str,
        initial: bool,
        scratch: bool,
        commit_rows: usize,
    ) -> DumpResult<()> {
        let count = self.rows.len() + self.events.len() + self.creations.len();
        if count == 0 {
            return Ok(());
        }
        self.max_buffer = self.max_buffer.max(count);
        let before = std::time::Instant::now();
        let query = if initial {
            "INSERT INTO history_initial VALUES(?,?,?,?,?,?,?,?,?,?,?,?)"
        } else {
            "INSERT INTO history_edit VALUES(?,?,?,?,?)"
        };
        let mut statement = connection.prepare_cached(query)?;
        for row in &self.rows {
            statement.execute(rusqlite::params_from_iter(row))?;
        }
        self.retained += self.rows.len();
        self.rows.clear();
        let mut statement =
            connection.prepare_cached("INSERT INTO history_page_event VALUES(?,?,?,?)")?;
        for row in &self.events {
            statement.execute(rusqlite::params_from_iter(row))?;
        }
        self.events.clear();
        let query = if scratch {
            "INSERT INTO history_creation_pending VALUES(?,?)"
        } else {
            "INSERT OR IGNORE INTO history_creation VALUES(?,?,?)"
        };
        let mut statement = connection.prepare_cached(query)?;
        for (page, created) in &self.creations {
            if scratch {
                statement.execute(rusqlite::params![page, created])?;
                self.scratch_rows += 1;
            } else {
                self.creation_rows += statement.execute(rusqlite::params![name, page, created])?;
            }
        }
        self.creations.clear();
        self.pending += count;
        self.maybe_commit(connection, commit_rows)?;
        self.sqlite_seconds += before.elapsed().as_secs_f64();
        Ok(())
    }
    fn maybe_commit(
        &mut self,
        connection: &rusqlite::Connection,
        commit_rows: usize,
    ) -> DumpResult<()> {
        if self.pending >= commit_rows {
            connection.execute_batch("COMMIT; BEGIN")?;
            self.commits += 1;
            self.pending = 0;
        }
        Ok(())
    }
    fn progress(&self, database: &str, name: &str, phase: &str, elapsed: f64, done: bool) {
        eprintln!("MediaWiki History {database}: {name} {phase}, {} righe lette; {} righe utili; {} creazioni duplicate in cache; {} commit; {elapsed:.1}s, SQLite {:.1}s; {:.0} righe/s; {} creazioni finali; {} append scratch; {} byte scratch SQLite; buffer massimo {}{}",
            self.scanned, self.retained + self.rows.len(), self.cache_hits, self.commits,
            self.sqlite_seconds, self.scanned as f64 / elapsed.max(0.001), self.creation_rows, self.scratch_rows, self.scratch_bytes, self.max_buffer,
            if done { "; COMPLETATO" } else { "" });
    }
}

#[cfg(feature = "cli")]
fn history_decimal(value: &str) -> DumpResult<Option<i64>> {
    // Python str.isdecimal accepts Unicode Nd, but not superscripts or other
    // numeric characters. Official IDs normally take the fast ASCII path.
    if value.is_empty() {
        return Ok(None);
    }
    if value.is_ascii() {
        return if value.bytes().all(|b| b.is_ascii_digit()) {
            Ok(Some(value.parse()?))
        } else {
            Ok(None)
        };
    }
    static DECIMAL: std::sync::OnceLock<Result<regex::Regex, regex::Error>> =
        std::sync::OnceLock::new();
    let decimal = DECIMAL
        .get_or_init(|| regex::Regex::new(r"^\p{Nd}$"))
        .as_ref()
        .map_err(|e| e.to_string())?;
    let mut normalized = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_digit() {
            normalized.push(character);
            continue;
        }
        if !decimal.is_match(&character.to_string()) {
            return Ok(None);
        }
        // Decimal digit blocks contain complete, ordered runs of ten digits;
        // adjoining mathematical styles can form a run of fifty.
        let mut code = character as u32;
        let mut digit = 0;
        while let Some(previous) = code.checked_sub(1).and_then(char::from_u32) {
            if !decimal.is_match(&previous.to_string()) {
                break;
            }
            digit = (digit + 1) % 10;
            code -= 1;
        }
        normalized.push(char::from(b'0' + digit));
    }
    Ok(Some(normalized.parse()?))
}

#[cfg(feature = "cli")]
fn scan_history(
    connection: &rusqlite::Connection,
    name: &str,
    data: &serde_json::Value,
    scan: &HistoryScan<'_>,
) -> DumpResult<()> {
    use rusqlite::types::Value as Sql;
    use rusqlite::{params, OptionalExtension};
    use sha2::Digest;
    use std::collections::{BTreeSet, HashMap};
    use std::io::{BufRead, BufReader};
    let initial = scan.users.is_none();
    let phase = if initial { "initial" } else { "activity" };
    let digest = data["sha256_local"]
        .as_str()
        .ok_or("Checksum History mancante")?;
    let suffix = if let Some(users) = scan.users {
        // Python's default JSON separators are part of the existing identity.
        let json = format!(
            "[{}]",
            users
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        format!("{:x}", sha2::Sha256::digest(json.as_bytes()))
    } else {
        HISTORY_RULE.to_owned()
    };
    let identity = format!("{digest}:{suffix}");
    let previous: Option<String> = connection
        .query_row(
            "SELECT sha256 FROM history_imported WHERE file=? AND phase=?",
            params![name, phase],
            |r| r.get(0),
        )
        .optional()?;
    if previous.as_deref() == Some(&identity) {
        eprintln!(
            "MediaWiki History {}: {name} {phase}, SALTA checkpoint completo",
            scan.database
        );
        return Ok(());
    }
    let table = if initial {
        "history_initial"
    } else {
        "history_edit"
    };
    connection.execute_batch("BEGIN")?;
    connection.execute(&format!("DELETE FROM {table} WHERE file=?"), [name])?;
    if initial {
        connection.execute("DELETE FROM history_page_event WHERE file=?", [name])?;
        connection.execute("DELETE FROM history_creation WHERE file=?", [name])?;
    }
    connection.execute(
        "DELETE FROM history_imported WHERE file=? AND phase=?",
        params![name, phase],
    )?;
    connection.execute_batch("COMMIT; BEGIN")?;
    let scratch = initial && scan.pages.len() > scan.options.creation_cache;
    if scratch {
        connection.execute_batch("CREATE TEMP TABLE history_creation_pending(page_id INTEGER NOT NULL,created TEXT NOT NULL)")?;
    }
    let result = (|| -> DumpResult<()> {
        let path = data["path"].as_str().ok_or("Percorso History mancante")?;
        let mut reader =
            BufReader::new(bzip2::read::MultiBzDecoder::new(std::fs::File::open(path)?));
        let started = std::time::Instant::now();
        let mut reported = started;
        let mut buffer = HistoryBuffer::default();
        // Exactly bounded LRU: one map and one ordered entry per identity,
        // including repeated hits. Eviction never discards conflicting evidence.
        let mut recent = HashMap::<(i64, String), usize>::new();
        let mut order = BTreeSet::<(usize, (i64, String))>::new();
        let mut line = String::new();
        let text = |s: &str| Sql::Text(s.to_owned());
        let optional = |s: &str| if s.is_empty() { Sql::Null } else { text(s) };
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            buffer.scanned += 1;
            if buffer.scanned % 10_000 == 0 && reported.elapsed().as_secs() >= 30 {
                buffer.progress(
                    scan.database,
                    name,
                    phase,
                    started.elapsed().as_secs_f64(),
                    false,
                );
                reported = std::time::Instant::now();
            }
            let row = line.trim_end_matches('\n').split('\t').collect::<Vec<_>>();
            if row.len() != 78 || row[0] != scan.database {
                return Err(format!(
                    "MediaWiki History: schema o wiki inattesi in {name}:{}",
                    buffer.scanned
                )
                .into());
            }
            if initial {
                let Some(page) = history_decimal(row[28]).ok().flatten() else {
                    continue;
                };
                if !scan.pages.contains(&page) {
                    continue;
                }
                if matches!(row[2], "page" | "revision") && !row[37].is_empty() {
                    let key = (page, row[37].to_owned());
                    if let Some(tick) = recent.insert(key.clone(), buffer.scanned) {
                        order.remove(&(tick, key.clone()));
                        buffer.cache_hits += 1;
                    } else {
                        buffer.creations.insert(key.clone());
                    }
                    order.insert((buffer.scanned, key));
                    if recent.len() > scan.options.creation_cache {
                        if let Some((_, key)) = order.pop_first() {
                            recent.remove(&key);
                        }
                    }
                }
                if row[2] == "page" && matches!(row[3], "create" | "import" | "merge") {
                    buffer.events.push(vec![
                        text(name),
                        Sql::Integer(page),
                        text(row[3]),
                        text(row[4]),
                    ]);
                } else if row[2] == "revision" && row[3] == "create" && row[61] == "0" {
                    let revision = history_decimal(row[60])?
                        .ok_or_else(|| format!("Revisione iniziale priva di ID: {page}"))?;
                    let user = history_decimal(row[6])?
                        .map(Sql::Integer)
                        .unwrap_or(Sql::Null);
                    let hidden = row[63]
                        .split(',')
                        .any(|part| matches!(part.trim(), "user" | "user_is_deleted"));
                    buffer.rows.push(vec![
                        text(name),
                        Sql::Integer(page),
                        Sql::Integer(revision),
                        text(row[4]),
                        user,
                        Sql::Integer(i64::from(hidden)),
                        optional(row[12]),
                        optional(row[77]),
                        optional(row[23]),
                        optional(row[37]),
                        optional(row[38]),
                        Sql::Integer(i64::from(row[76] == "true")),
                    ]);
                }
            } else if row[2] == "revision" && row[3] == "create" {
                if let Some(user) = history_decimal(row[6]).ok().flatten() {
                    if scan.users.is_some_and(|users| users.contains(&user)) {
                        let Some(revision) = history_decimal(row[60])? else {
                            continue;
                        };
                        let namespace = if row[31].is_empty() { row[33] } else { row[31] };
                        if history_decimal(namespace.trim_start_matches('-'))?.is_some() {
                            let namespace = namespace.parse::<i64>()?;
                            buffer.rows.push(vec![
                                text(name),
                                Sql::Integer(revision),
                                Sql::Integer(user),
                                text(row[4]),
                                Sql::Integer(namespace),
                            ]);
                        }
                    }
                }
            }
            if buffer.rows.len() >= 5000
                || buffer.events.len() >= 5000
                || buffer.creations.len() >= 5000
            {
                buffer.flush(connection, name, initial, scratch, scan.options.commit_rows)?;
            }
        }
        buffer.flush(connection, name, initial, scratch, scan.options.commit_rows)?;
        if scratch {
            let before = std::time::Instant::now();
            connection.execute_batch("CREATE TEMP TABLE history_creation_ordered AS SELECT DISTINCT page_id,created FROM history_creation_pending ORDER BY page_id,created")?;
            buffer.scratch_bytes = connection
                .query_row("PRAGMA temp.page_count", [], |r| r.get::<_, u64>(0))?
                * connection.query_row("PRAGMA temp.page_size", [], |r| r.get::<_, u64>(0))?;
            connection.execute_batch("DROP TABLE history_creation_pending")?;
            buffer.sqlite_seconds += before.elapsed().as_secs_f64();
            let mut first = 1;
            loop {
                let before = std::time::Instant::now();
                let inserted = connection.execute("INSERT INTO history_creation SELECT ?,page_id,created FROM history_creation_ordered WHERE rowid>=? AND rowid<? ORDER BY rowid",params![name,first,first+5000])?;
                buffer.creation_rows += inserted;
                buffer.pending += inserted;
                buffer.maybe_commit(connection, scan.options.commit_rows)?;
                buffer.sqlite_seconds += before.elapsed().as_secs_f64();
                if inserted < 5000 {
                    break;
                }
                first += 5000;
                if reported.elapsed().as_secs() >= 30 {
                    buffer.progress(
                        scan.database,
                        name,
                        phase,
                        started.elapsed().as_secs_f64(),
                        false,
                    );
                    reported = std::time::Instant::now();
                }
            }
        }
        let before = std::time::Instant::now();
        connection.execute(
            "INSERT INTO history_imported VALUES(?,?,?)",
            params![name, phase, identity],
        )?;
        connection.execute_batch("COMMIT")?;
        buffer.commits += 1;
        buffer.sqlite_seconds += before.elapsed().as_secs_f64();
        buffer.progress(
            scan.database,
            name,
            phase,
            started.elapsed().as_secs_f64(),
            true,
        );
        Ok(())
    })();
    // Preserve committed incomplete rows for diagnostics, but never their marker.
    if !connection.is_autocommit() {
        connection.execute_batch("ROLLBACK")?;
    }
    if scratch {
        connection.flush_prepared_statement_cache();
        connection.execute_batch("DROP TABLE IF EXISTS history_creation_pending; DROP TABLE IF EXISTS history_creation_ordered")?;
    }
    result
}

#[cfg(feature = "cli")]
fn history_records(
    connection: &rusqlite::Connection,
    pages: &std::collections::BTreeSet<i64>,
    database: &str,
) -> DumpResult<Vec<serde_json::Value>> {
    use serde_json::{json, Value};
    use std::collections::{HashMap, HashSet};
    #[derive(Default)]
    struct Events {
        creates: usize,
        first: String,
        imported: bool,
    }
    let mut dates = HashMap::<i64, HashSet<Option<String>>>::new();
    let mut query = connection.prepare("SELECT DISTINCT page_id,created FROM history_creation")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let raw: String = row.get(1)?;
        dates
            .entry(row.get(0)?)
            .or_default()
            .insert(history_timestamp(Some(&raw)));
    }
    let mut initial = HashMap::<i64, (usize, Value)>::new();
    let mut query = connection.prepare("SELECT page_id,revision_id,at,user_id,hidden,groups,tags,registration,page_created,page_first,before_creation FROM history_initial ORDER BY page_id,at,revision_id")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let entry = initial.entry(row.get(0)?).or_insert((0, Value::Null));
        entry.0 += 1;
        if entry.0 == 1 {
            entry.1 = json!({"revision":row.get::<_,i64>(1)?,"at":row.get::<_,String>(2)?,
                "user":row.get::<_,Option<i64>>(3)?,"hidden":row.get::<_,i64>(4)? != 0,
                "groups":row.get::<_,Option<String>>(5)?,"tags":row.get::<_,Option<String>>(6)?,
                "registration":row.get::<_,Option<String>>(7)?,"created":row.get::<_,Option<String>>(8)?,
                "first":row.get::<_,Option<String>>(9)?,"before":row.get::<_,i64>(10)? != 0});
        }
    }
    let mut events = HashMap::<i64, Events>::new();
    let mut query = connection.prepare("SELECT page_id,event,at FROM history_page_event")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let event: String = row.get(1)?;
        let entry = events.entry(row.get(0)?).or_default();
        if event == "create" {
            entry.creates += 1;
            if entry.creates == 1 {
                entry.first = row.get(2)?;
            }
        }
        entry.imported |= matches!(event.as_str(), "import" | "merge");
    }
    let mut records = Vec::with_capacity(pages.len());
    for page in pages {
        let dates = dates.remove(page).unwrap_or_default();
        let created = if dates.len() == 1 {
            dates.iter().next().cloned().flatten()
        } else {
            None
        };
        let date_status = if created.is_some() {
            "reconstructed_history"
        } else if dates.is_empty() {
            "missing_history_creation"
        } else {
            "ambiguous_history_creation"
        };
        let mut record = json!({"page_id":page,"first_revision_id":null,"article_created_at":null,
            "creator_key":null,"creator_user_id":null,"creator_attribution":"history_missing_initial_revision",
            "creator_groups_at_creation":null,"creator_groups_status":"unknown",
            "creator_continuity_months_before_creation":null,"creator_continuity_bucket_before_creation":null,
            "creator_prior_edits_main_history":null,"creator_prior_edits_other_history":null,
            "creator_history_status":"unavailable","page_created_at":created,"creation_date_status":date_status,
            "creator_registered_at":null,"creator_tenure_days":null,"creator_prior_articles":null,
            "creator_account_type":null,"creator_is_bot_now":null});
        if let Some((count, revision)) = initial.remove(page) {
            if count > 1 {
                record["creator_attribution"] = json!("history_ambiguous_initial_revisions");
            } else {
                let at = revision["at"]
                    .as_str()
                    .ok_or("Timestamp History mancante")?;
                record["first_revision_id"] = revision["revision"].clone();
                record["article_created_at"] = json!(history_timestamp(Some(at)));
                let event = events.remove(page).unwrap_or_default();
                let mismatch = |key: &str| {
                    revision[key]
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s != at)
                };
                let attribution = if record["article_created_at"].is_null()
                    || mismatch("created")
                    || mismatch("first")
                {
                    "history_inconsistent_creation_dates"
                } else if event.imported || revision["before"] == true || event.creates > 1 {
                    "history_imported_or_ambiguous"
                } else if event.creates > 0 && event.first != at {
                    "history_create_event_mismatch"
                } else if revision["hidden"] == true || revision["user"].is_null() {
                    "history_hidden_or_unregistered_creator"
                } else {
                    "history_initial_revision_observed"
                };
                record["creator_attribution"] = json!(attribution);
                if attribution == "history_initial_revision_observed" {
                    let user = revision["user"]
                        .as_i64()
                        .ok_or("Identità History mancante")?;
                    record["creator_user_id"] = json!(user);
                    record["creator_key"] = json!(format!("{database}:history-user:{user}"));
                    if let Some(groups) = revision["groups"].as_str().filter(|s| !s.is_empty()) {
                        record["creator_groups_at_creation"] =
                            json!(groups.split(',').collect::<Vec<_>>());
                        record["creator_groups_status"] = json!("observed");
                    }
                    record["creator_history_status"] = json!("pending_activity");
                    record["creator_registered_at"] =
                        json!(history_timestamp(revision["registration"].as_str()));
                    if let Some(tags) = revision["tags"].as_str().filter(|s| !s.is_empty()) {
                        record["creation_tags_history"] =
                            json!(tags.split(',').collect::<Vec<_>>());
                    }
                }
            }
        }
        records.push(record);
    }
    Ok(records)
}

#[cfg(feature = "cli")]
fn history_creator_metrics(
    connection: &rusqlite::Connection,
    records: &mut [serde_json::Value],
    users: &std::collections::BTreeSet<i64>,
) -> DumpResult<()> {
    use serde_json::{json, Value};
    use std::collections::{BTreeMap, HashSet};
    let mut by_user = BTreeMap::<i64, Vec<usize>>::new();
    for (index, record) in records.iter().enumerate() {
        if let Some(user) = record["creator_user_id"].as_i64() {
            by_user.entry(user).or_default().push(index);
        }
    }
    let finish = |user: i64, edits: Vec<(String, i32)>, records: &mut [Value]| -> DumpResult<()> {
        let indices = by_user
            .get(&user)
            .ok_or("Attività History priva di autore attribuito")?;
        let mut group = Vec::new();
        let mut dates = Vec::new();
        for &index in indices {
            let record = &mut records[index];
            if let Some(date) = record["page_created_at"].as_str() {
                group.push(index);
                dates.push(date.replace('T', " ").trim_end_matches("+00:00").to_owned());
            } else {
                record["creator_history_status"] = json!("observed_surviving_history");
            }
        }
        match history_activity(edits, dates) {
            Ok(summaries) => {
                for (index, (main, other, months, bucket)) in group.into_iter().zip(summaries) {
                    let record = &mut records[index];
                    record["creator_prior_edits_main_history"] = json!(main);
                    record["creator_prior_edits_other_history"] = json!(other);
                    record["creator_continuity_months_before_creation"] = json!(months);
                    record["creator_continuity_bucket_before_creation"] = json!(bucket);
                    record["creator_history_status"] = json!("observed_surviving_history");
                }
            }
            Err(_) => {
                for index in group {
                    records[index]["creator_history_status"] = json!("invalid_history_timestamp");
                }
            }
        }
        Ok(())
    };
    let mut query =
        connection.prepare("SELECT user_id,at,namespace FROM history_edit ORDER BY user_id,at")?;
    let mut rows = query.query([])?;
    let mut previous = None;
    let mut edits = Vec::new();
    let mut finished = HashSet::new();
    while let Some(row) = rows.next()? {
        let user = row.get::<_, i64>(0)?;
        if let Some(previous) = previous.filter(|&previous| previous != user) {
            finish(previous, std::mem::take(&mut edits), records)?;
            finished.insert(previous);
        }
        previous = Some(user);
        edits.push((row.get(1)?, row.get(2)?));
    }
    if let Some(previous) = previous {
        finish(previous, edits, records)?;
        finished.insert(previous);
    }
    for &user in users {
        if !finished.contains(&user) {
            finish(user, Vec::new(), records)?;
        }
    }
    for indices in by_user.values() {
        let mut dates = indices
            .iter()
            .filter_map(|&index| {
                let record = &records[index];
                (record["creator_history_status"] == "observed_surviving_history")
                    .then(|| record["page_created_at"].as_str().map(str::to_owned))
                    .flatten()
            })
            .collect::<Vec<_>>();
        dates.sort();
        for &index in indices {
            let record = &mut records[index];
            if record["creator_history_status"] == "observed_surviving_history" {
                if let Some(date) = record["page_created_at"].as_str() {
                    record["creator_prior_articles"] =
                        json!(dates.partition_point(|prior| prior.as_str() < date));
                }
            }
        }
    }
    Ok(())
}

/// Reconstruct History supplements from verified partitions and existing SQL
/// staging. No graph, topics or collected research input is regenerated.
#[cfg(feature = "cli")]
pub fn import_history(
    manifest_path: &std::path::Path,
    output: &std::path::Path,
    work_dir: &std::path::Path,
    options: HistoryOptions,
) -> DumpResult<usize> {
    use rusqlite::OptionalExtension;
    use serde_json::json;
    use std::collections::{BTreeSet, HashMap};
    use std::io::Write;
    if !(1..=65536).contains(&options.cache_mib) {
        return Err("Cache History: usare da 1 a 65536 MiB".into());
    }
    if !(1..=10_000_000).contains(&options.commit_rows) {
        return Err("Commit History: usare da 1 a 10000000 record".into());
    }
    if options.creation_cache == 0 {
        return Err("Cache delle identità History vuota".into());
    }
    let manifest: HistoryManifest = serde_json::from_slice(&std::fs::read(manifest_path)?)?;
    let database = crate::wiki_database(&manifest.language, manifest.database.as_deref())?;
    let stage = work_dir.join(format!("{database}-{}.sqlite", manifest.sql_release));
    if !stage.is_file() {
        return Err("Importare prima il grafo SQL della stessa wiki".into());
    }
    let files = manifest
        .files
        .iter()
        .filter(|(name, _)| name.ends_with(".tsv.bz2"))
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Err("Manifest senza partizioni MediaWiki History".into());
    }
    let resolved_output = resolved_path(output)?;
    if resolved_output == resolved_path(manifest_path)? || resolved_output == resolved_path(&stage)?
    {
        return Err("Output e input/staging devono essere distinti".into());
    }
    for (_, data) in &manifest.files {
        if let Some(path) = data["path"].as_str() {
            if resolved_output == resolved_path(std::path::Path::new(path))? {
                return Err("Output e input/staging devono essere distinti".into());
            }
        }
    }
    let connection =
        rusqlite::Connection::open_with_flags(&stage, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.execute_batch(&format!("PRAGMA temp_store=FILE; PRAGMA cache_size=-{};
        CREATE TABLE IF NOT EXISTS history_imported(file TEXT NOT NULL,phase TEXT NOT NULL,sha256 TEXT NOT NULL,PRIMARY KEY(file,phase));
        CREATE TABLE IF NOT EXISTS history_initial(file TEXT NOT NULL,page_id INTEGER NOT NULL,revision_id INTEGER NOT NULL,at TEXT NOT NULL,user_id INTEGER,hidden INTEGER NOT NULL,groups TEXT,tags TEXT,registration TEXT,page_created TEXT,page_first TEXT,before_creation INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS history_page_event(file TEXT NOT NULL,page_id INTEGER NOT NULL,event TEXT NOT NULL,at TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS history_edit(file TEXT NOT NULL,revision_id INTEGER NOT NULL,user_id INTEGER NOT NULL,at TEXT NOT NULL,namespace INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS history_creation(file TEXT NOT NULL,page_id INTEGER NOT NULL,created TEXT NOT NULL,PRIMARY KEY(file,page_id,created));", options.cache_mib*1024))?;
    let file = |name: &str| {
        manifest
            .files
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, data)| data)
    };
    if SQL_TABLES.iter().all(|name| file(name).is_some()) {
        for name in SQL_TABLES {
            let digest: Option<String> = connection
                .query_row(
                    "SELECT sha256 FROM imported WHERE table_name=?",
                    [name],
                    |r| r.get(0),
                )
                .optional()?;
            if digest.as_deref() != file(name).and_then(|data| data["sha256_local"].as_str()) {
                return Err("Staging SQL non verificato per la cronologia".into());
            }
        }
    }
    let pages = connection
        .prepare("SELECT id FROM dump_page WHERE ns=0 AND redirect=0")?
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<BTreeSet<_>, _>>()?;
    if pages.is_empty() {
        return Err("Staging SQL incompleto".into());
    }
    eprintln!("MediaWiki History {database}: cache SQLite {} MiB; commit ogni circa {} record; batch Rust fino a 5000 per tipo",options.cache_mib,options.commit_rows);
    for (name, data) in &files {
        let path = std::path::Path::new(data["path"].as_str().ok_or("Percorso History mancante")?);
        let digest = data["sha256_local"]
            .as_str()
            .ok_or("Checksum History mancante")?;
        if !path.is_file()
            || Some(path.metadata()?.len()) != data["bytes"].as_u64()
            || DumpClient::<WikimediaHttp>::verified_sha256(path, None, Some(digest))?.is_none()
        {
            return Err(format!("Partizione MediaWiki History modificata: {name}").into());
        }
        scan_history(
            &connection,
            name,
            data,
            &HistoryScan {
                database: &database,
                pages: &pages,
                users: None,
                options,
            },
        )?;
    }
    let mut records = history_records(&connection, &pages, &database)?;
    let snapshot =
        history_timestamp(file("user_groups").and_then(|data| data["job_updated"].as_str()))
            .map(Ok)
            .unwrap_or_else(|| {
                let date = chrono::NaiveDate::parse_from_str(&manifest.sql_release, "%Y%m%d")?;
                Ok::<_, chrono::ParseError>(format!("{}T00:00:00+00:00", date.format("%Y-%m-%d")))
            })?;
    let mut bots = HashMap::<i64, Vec<Option<String>>>::new();
    let has_imported = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE name='imported'",
            [],
            |r| r.get::<_, i32>(0),
        )
        .optional()?
        .is_some();
    let has_bots = has_imported
        && connection
            .query_row(
                "SELECT 1 FROM imported WHERE table_name='user_groups'",
                [],
                |r| r.get::<_, i32>(0),
            )
            .optional()?
            .is_some();
    if has_bots {
        let mut query =
            connection.prepare("SELECT user_id,expiry FROM dump_group WHERE name='bot'")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            bots.entry(row.get(0)?).or_default().push(row.get(1)?);
        }
    }
    for record in &mut records {
        if let Some(user) = record["creator_user_id"].as_i64().filter(|_| has_bots) {
            let (mut bot, mut unknown) = (false, false);
            for expiry in bots.get(&user).into_iter().flatten() {
                if let Some(expiry) = expiry {
                    let parsed = if expiry.len() == 14
                        && expiry.bytes().all(|b| b.is_ascii_digit())
                        && !expiry.starts_with("0000")
                    {
                        chrono::NaiveDateTime::parse_from_str(expiry, "%Y%m%d%H%M%S")
                            .ok()
                            .and_then(|dt| {
                                history_timestamp(Some(&dt.format("%Y-%m-%d %H:%M:%S").to_string()))
                            })
                    } else {
                        None
                    };
                    if let Some(expiry) = parsed {
                        bot |= expiry > snapshot;
                    } else {
                        unknown = true;
                    }
                } else {
                    bot = true;
                }
            }
            if bot || !unknown {
                record["creator_is_bot_now"] = json!(bot);
                record["creator_account_type"] = json!(if bot { "bot" } else { "no_bot_flag" });
            }
        }
        if let (Some(created), Some(registered)) = (
            record["page_created_at"].as_str(),
            record["creator_registered_at"].as_str(),
        ) {
            let parse = |date: &str| chrono::DateTime::parse_from_rfc3339(date);
            let micros = (parse(created)? - parse(registered)?)
                .num_microseconds()
                .ok_or("Intervallo History fuori limite")?;
            if micros >= 0 {
                record["creator_tenure_days"] = json!(micros as f64 / 1_000_000.0 / 86400.0);
            }
        }
    }
    let users = records
        .iter()
        .filter_map(|r| r["creator_user_id"].as_i64())
        .collect::<BTreeSet<_>>();
    for (name, data) in &files {
        scan_history(
            &connection,
            name,
            data,
            &HistoryScan {
                database: &database,
                pages: &pages,
                users: Some(&users),
                options,
            },
        )?;
    }
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS history_edit_user_time ON history_edit(user_id,at)",
    )?;
    history_creator_metrics(&connection, &mut records, &users)?;
    let history_files = files
        .iter()
        .map(|(name, data)| ((*name).clone(), data["sha256_local"].clone()))
        .collect::<serde_json::Map<_, _>>();
    let metadata = json!({"language":manifest.language,"database":database,"creation_rule":HISTORY_RULE,"creator_metrics_rule":HISTORY_RULE,
        "account_type_snapshot_at":snapshot,"source":format!("MediaWiki History dumps / {database}"),
        "sql_release":manifest.sql_release,"history_release":manifest.history_release,"complete":true,
        "creator_definition":"single visible parent-zero revision in reconstructed history; not an observed creation event",
        "experience_scope":"public surviving revisions strictly before reconstructed page creation; prior articles restricted to the census",
        "continuity_rule":"consecutive completed precreation calendar months with at least five edits; Vital Signs stability adaptation",
        "group_scope":"nonempty reconstructed historical group list at first revision; blank means unknown","history_files":history_files});
    write_atomic(output, |file| {
        let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file);
        writer.write_all(b"{\"metadata\":")?;
        serde_json::to_writer(&mut writer, &metadata)?;
        writer.write_all(b",\"pages\":[")?;
        for (index, record) in records.iter().enumerate() {
            if index > 0 {
                writer.write_all(b",")?;
            }
            serde_json::to_writer(&mut writer, record)?;
        }
        writer.write_all(b"]}\n")?;
        writer.flush()?;
        Ok(())
    })?;
    eprintln!(
        "Cronologia da dump: {} voci; {}",
        records.len(),
        output.display()
    );
    Ok(records.len())
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

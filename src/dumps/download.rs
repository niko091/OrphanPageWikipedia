#[cfg(feature = "cli")]
use super::write_atomic;
use super::{DUMP_BASE, SQL_TABLES};

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
        crate::runtime::phase("hash.file", || {
            crate::runtime::check_cancelled()?;
            use sha2::Digest;
            use std::io::Read;
            let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
            let mut one = sha1.map(|_| sha1::Sha1::new());
            let mut two = sha2::Sha256::new();
            let mut buffer = vec![0; 1024 * 1024];
            let mut scanned = 0u64;
            loop {
                crate::runtime::check_cancelled()?;
                let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
                if count == 0 {
                    break;
                }
                two.update(&buffer[..count]);
                scanned += count as u64;
                if let Some(one) = &mut one {
                    one.update(&buffer[..count]);
                }
            }
            let checksum = format!("{:x}", two.finalize());
            crate::runtime::phase_counts(None, Some(scanned));
            if one
                .zip(sha1)
                .is_some_and(|(one, expected)| format!("{:x}", one.finalize()) != expected)
                || sha256.is_some_and(|expected| checksum != expected)
            {
                return Ok(None);
            }
            Ok(Some(checksum))
        })
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
        crate::runtime::phase("download.file", || {
            crate::runtime::check_cancelled()?;
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
                crate::runtime::check_cancelled()?;
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
        })
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
        crate::runtime::phase("download.bzip2_integrity", || {
            crate::runtime::check_cancelled()?;
            let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
            let mut stream = bzip2::read::MultiBzDecoder::new(file);
            std::io::copy(&mut stream, &mut std::io::sink()).map_err(|error| error.to_string())?;
            Ok(())
        })
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

#[cfg(feature = "cli")]
use super::{resolved_path, write_atomic, DumpClient, WikimediaHttp};
#[cfg(feature = "cli")]
use super::{DumpResult, SQL_TABLES};

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

#[cfg(feature = "cli")]
pub(super) fn history_timestamp(value: Option<&str>) -> Option<String> {
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
pub(crate) const HISTORY_RULE: &str = "mediawiki_history_page_creation_v2";

// Preserve partition order without requiring an ordered JSON-map dependency.
#[cfg(feature = "cli")]
#[derive(serde::Deserialize)]
pub(super) struct HistoryManifest {
    language: String,
    database: Option<String>,
    sql_release: String,
    history_release: String,
    #[serde(deserialize_with = "history_files")]
    files: Vec<(String, serde_json::Value)>,
}

#[cfg(feature = "cli")]
pub(super) fn history_files<'de, D: serde::Deserializer<'de>>(
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
pub(super) struct HistoryScan<'a> {
    database: &'a str,
    pages: &'a std::collections::BTreeSet<i64>,
    users: Option<&'a std::collections::BTreeSet<i64>>,
    options: HistoryOptions,
}

#[cfg(feature = "cli")]
#[derive(Default)]
pub(super) struct HistoryBuffer {
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
        crate::runtime::phase_counts(Some(self.scanned as u64), None);
        eprintln!("MediaWiki History {database}: {name} {phase}, {} righe lette; {} righe utili; {} creazioni duplicate in cache; {} commit; {elapsed:.1}s, SQLite {:.1}s; {:.0} righe/s; {} creazioni finali; {} append scratch; {} byte scratch SQLite; buffer massimo {}{}",
            self.scanned, self.retained + self.rows.len(), self.cache_hits, self.commits,
            self.sqlite_seconds, self.scanned as f64 / elapsed.max(0.001), self.creation_rows, self.scratch_rows, self.scratch_bytes, self.max_buffer,
            if done { "; COMPLETATO" } else { "" });
    }
}

#[cfg(feature = "cli")]
pub(super) fn history_decimal(value: &str) -> DumpResult<Option<i64>> {
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
pub(super) fn scan_history(
    connection: &rusqlite::Connection,
    name: &str,
    data: &serde_json::Value,
    scan: &HistoryScan<'_>,
) -> DumpResult<()> {
    crate::runtime::phase("history.partition", || {
        crate::runtime::check_cancelled()?;
        use rusqlite::types::Value as Sql;
        use rusqlite::{params, OptionalExtension};
        use sha2::Digest;
        use std::collections::{BTreeSet, HashMap};
        use std::io::{BufRead, BufReader};
        let initial = scan.users.is_none();
        let phase = if initial { "initial" } else { "activity" };
        crate::runtime::phase_tag("phase", serde_json::json!(phase));
        crate::runtime::phase_tag("partition", serde_json::json!(name));
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
                crate::runtime::check_cancelled()?;
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
                    crate::runtime::check_cancelled()?;
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
    })
}

#[cfg(feature = "cli")]
#[derive(serde::Serialize)]
pub(super) struct HistoryRecord {
    page_id: i64,
    first_revision_id: Option<i64>,
    article_created_at: Option<String>,
    creator_key: Option<String>,
    creator_user_id: Option<i64>,
    creator_attribution: &'static str,
    creator_groups_at_creation: Option<Vec<String>>,
    creator_groups_status: &'static str,
    creator_continuity_months_before_creation: Option<usize>,
    creator_continuity_bucket_before_creation: Option<String>,
    creator_prior_edits_main_history: Option<usize>,
    creator_prior_edits_other_history: Option<usize>,
    creator_history_status: &'static str,
    page_created_at: Option<String>,
    creation_date_status: &'static str,
    creator_registered_at: Option<String>,
    creator_tenure_days: Option<f64>,
    creator_prior_articles: Option<usize>,
    creator_account_type: Option<&'static str>,
    creator_is_bot_now: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    creation_tags_history: Option<Vec<String>>,
}

#[cfg(feature = "cli")]
pub(super) struct InitialRevision {
    revision: i64,
    at: String,
    user: Option<i64>,
    hidden: bool,
    groups: Option<String>,
    tags: Option<String>,
    registration: Option<String>,
    created: Option<String>,
    first: Option<String>,
    before: bool,
}

#[cfg(feature = "cli")]
pub(super) fn history_records(
    connection: &rusqlite::Connection,
    pages: &std::collections::BTreeSet<i64>,
    database: &str,
) -> DumpResult<Vec<HistoryRecord>> {
    crate::runtime::phase("history.records", || {
        crate::runtime::check_cancelled()?;
        use std::collections::{HashMap, HashSet};
        #[derive(Default)]
        struct Events {
            creates: usize,
            first: String,
            imported: bool,
        }
        let mut dates = HashMap::<i64, HashSet<Option<String>>>::new();
        let mut query =
            connection.prepare("SELECT DISTINCT page_id,created FROM history_creation")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let raw: String = row.get(1)?;
            dates
                .entry(row.get(0)?)
                .or_default()
                .insert(history_timestamp(Some(&raw)));
        }
        let mut initial = HashMap::<i64, (usize, InitialRevision)>::new();
        let mut query = connection.prepare("SELECT page_id,revision_id,at,user_id,hidden,groups,tags,registration,page_created,page_first,before_creation FROM history_initial ORDER BY page_id,at,revision_id")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            match initial.entry(row.get(0)?) {
                std::collections::hash_map::Entry::Occupied(mut entry) => entry.get_mut().0 += 1,
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert((
                        1,
                        InitialRevision {
                            revision: row.get(1)?,
                            at: row.get(2)?,
                            user: row.get(3)?,
                            hidden: row.get::<_, i64>(4)? != 0,
                            groups: row.get(5)?,
                            tags: row.get(6)?,
                            registration: row.get(7)?,
                            created: row.get(8)?,
                            first: row.get(9)?,
                            before: row.get::<_, i64>(10)? != 0,
                        },
                    ));
                }
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
            let mut record = HistoryRecord {
                page_id: *page,
                first_revision_id: None,
                article_created_at: None,
                creator_key: None,
                creator_user_id: None,
                creator_attribution: "history_missing_initial_revision",
                creator_groups_at_creation: None,
                creator_groups_status: "unknown",
                creator_continuity_months_before_creation: None,
                creator_continuity_bucket_before_creation: None,
                creator_prior_edits_main_history: None,
                creator_prior_edits_other_history: None,
                creator_history_status: "unavailable",
                page_created_at: created,
                creation_date_status: date_status,
                creator_registered_at: None,
                creator_tenure_days: None,
                creator_prior_articles: None,
                creator_account_type: None,
                creator_is_bot_now: None,
                creation_tags_history: None,
            };
            if let Some((count, revision)) = initial.remove(page) {
                if count > 1 {
                    record.creator_attribution = "history_ambiguous_initial_revisions";
                } else {
                    let at = revision.at.as_str();
                    record.first_revision_id = Some(revision.revision);
                    record.article_created_at = history_timestamp(Some(at));
                    let event = events.remove(page).unwrap_or_default();
                    let mismatch =
                        |date: Option<&str>| date.is_some_and(|s| !s.is_empty() && s != at);
                    let attribution = if record.article_created_at.is_none()
                        || mismatch(revision.created.as_deref())
                        || mismatch(revision.first.as_deref())
                    {
                        "history_inconsistent_creation_dates"
                    } else if event.imported || revision.before || event.creates > 1 {
                        "history_imported_or_ambiguous"
                    } else if event.creates > 0 && event.first != at {
                        "history_create_event_mismatch"
                    } else if revision.hidden || revision.user.is_none() {
                        "history_hidden_or_unregistered_creator"
                    } else {
                        "history_initial_revision_observed"
                    };
                    record.creator_attribution = attribution;
                    if attribution == "history_initial_revision_observed" {
                        let user = revision.user.ok_or("Identità History mancante")?;
                        record.creator_user_id = Some(user);
                        record.creator_key = Some(format!("{database}:history-user:{user}"));
                        if let Some(groups) = revision.groups.as_deref().filter(|s| !s.is_empty()) {
                            record.creator_groups_at_creation =
                                Some(groups.split(',').map(str::to_owned).collect());
                            record.creator_groups_status = "observed";
                        }
                        record.creator_history_status = "pending_activity";
                        record.creator_registered_at =
                            history_timestamp(revision.registration.as_deref());
                        if let Some(tags) = revision.tags.as_deref().filter(|s| !s.is_empty()) {
                            record.creation_tags_history =
                                Some(tags.split(',').map(str::to_owned).collect());
                        }
                    }
                }
            }
            records.push(record);
        }
        Ok(records)
    })
}

#[cfg(feature = "cli")]
pub(super) fn history_creator_metrics(
    connection: &rusqlite::Connection,
    records: &mut [HistoryRecord],
    users: &std::collections::BTreeSet<i64>,
) -> DumpResult<()> {
    crate::runtime::phase("history.creator_metrics", || {
        crate::runtime::check_cancelled()?;
        use std::collections::{BTreeMap, HashSet};
        let mut by_user = BTreeMap::<i64, Vec<usize>>::new();
        for (index, record) in records.iter().enumerate() {
            if let Some(user) = record.creator_user_id {
                by_user.entry(user).or_default().push(index);
            }
        }
        let finish = |user: i64,
                      edits: Vec<(String, i32)>,
                      records: &mut [HistoryRecord]|
         -> DumpResult<()> {
            let indices = by_user
                .get(&user)
                .ok_or("Attività History priva di autore attribuito")?;
            let mut group = Vec::new();
            let mut dates = Vec::new();
            for &index in indices {
                let record = &mut records[index];
                if let Some(date) = record.page_created_at.as_deref() {
                    group.push(index);
                    dates.push(date.replace('T', " ").trim_end_matches("+00:00").to_owned());
                } else {
                    record.creator_history_status = "observed_surviving_history";
                }
            }
            match history_activity(edits, dates) {
                Ok(summaries) => {
                    for (index, (main, other, months, bucket)) in group.into_iter().zip(summaries) {
                        let record = &mut records[index];
                        record.creator_prior_edits_main_history = Some(main);
                        record.creator_prior_edits_other_history = Some(other);
                        record.creator_continuity_months_before_creation = Some(months);
                        record.creator_continuity_bucket_before_creation = Some(bucket);
                        record.creator_history_status = "observed_surviving_history";
                    }
                }
                Err(_) => {
                    for index in group {
                        records[index].creator_history_status = "invalid_history_timestamp";
                    }
                }
            }
            Ok(())
        };
        let mut query = connection
            .prepare("SELECT user_id,at,namespace FROM history_edit ORDER BY user_id,at")?;
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
                    (record.creator_history_status == "observed_surviving_history")
                        .then(|| record.page_created_at.as_deref().map(str::to_owned))
                        .flatten()
                })
                .collect::<Vec<_>>();
            dates.sort();
            for &index in indices {
                let record = &mut records[index];
                if record.creator_history_status == "observed_surviving_history" {
                    if let Some(date) = record.page_created_at.as_deref() {
                        record.creator_prior_articles =
                            Some(dates.partition_point(|prior| prior.as_str() < date));
                    }
                }
            }
        }
        Ok(())
    })
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
    crate::runtime::phase("history.import", || {
        crate::runtime::check_cancelled()?;
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
        if resolved_output == resolved_path(manifest_path)?
            || resolved_output == resolved_path(&stage)?
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
        let connection = rusqlite::Connection::open_with_flags(
            &stage,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
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
            let path =
                std::path::Path::new(data["path"].as_str().ok_or("Percorso History mancante")?);
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
                    Ok::<_, chrono::ParseError>(format!(
                        "{}T00:00:00+00:00",
                        date.format("%Y-%m-%d")
                    ))
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
            if let Some(user) = record.creator_user_id.filter(|_| has_bots) {
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
                                    history_timestamp(Some(
                                        &dt.format("%Y-%m-%d %H:%M:%S").to_string(),
                                    ))
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
                    record.creator_is_bot_now = Some(bot);
                    record.creator_account_type = Some(if bot { "bot" } else { "no_bot_flag" });
                }
            }
            if let (Some(created), Some(registered)) = (
                record.page_created_at.as_deref(),
                record.creator_registered_at.as_deref(),
            ) {
                let parse = |date: &str| chrono::DateTime::parse_from_rfc3339(date);
                let micros = (parse(created)? - parse(registered)?)
                    .num_microseconds()
                    .ok_or("Intervallo History fuori limite")?;
                if micros >= 0 {
                    record.creator_tenure_days = Some(micros as f64 / 1_000_000.0 / 86400.0);
                }
            }
        }
        let users = records
            .iter()
            .filter_map(|r| r.creator_user_id)
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
    })
}

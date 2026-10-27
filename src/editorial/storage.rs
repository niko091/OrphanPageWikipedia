use super::*;

#[derive(Clone)]
pub enum ReviewDatabase {
    Sqlite(PathBuf),
    Mysql(mysql::Opts),
    Unconfigured,
}

impl ReviewDatabase {
    pub fn from_environment(root: &Path, sqlite: Option<PathBuf>) -> Result<Self, String> {
        let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        if let Some(database) = env("ORPHANWIKI_REVIEW_MYSQL_DATABASE") {
            let mut defaults = BTreeMap::new();
            if let Some(path) = env("ORPHANWIKI_REVIEW_MYSQL_DEFAULTS_FILE") {
                let text = std::fs::read_to_string(path)
                    .map_err(|_| "File di configurazione ToolsDB non leggibile")?;
                defaults = client_defaults(&text)?;
            }
            let user = env("TOOL_TOOLSDB_USER").or_else(|| defaults.remove("user"));
            let password = env("TOOL_TOOLSDB_PASSWORD").or_else(|| defaults.remove("password"));
            if user.is_none() || password.is_none() {
                return Err("Configurazione ToolsDB incompleta".into());
            }
            let host = env("ORPHANWIKI_REVIEW_MYSQL_HOST")
                .or_else(|| defaults.remove("host"))
                .unwrap_or_else(|| "tools.db.svc.wikimedia.cloud".into());
            let mut options = mysql::OptsBuilder::new()
                .db_name(Some(database))
                .ip_or_hostname(Some(host))
                .user(user)
                .pass(password)
                .tcp_connect_timeout(Some(Duration::from_secs(5)))
                .read_timeout(Some(Duration::from_secs(10)))
                .write_timeout(Some(Duration::from_secs(10)))
                .init(vec!["SET NAMES utf8mb4"]);
            if let Some(port) = defaults.remove("port") {
                options = options.tcp_port(port.parse().map_err(|_| "Porta ToolsDB non valida")?);
            }
            if let Some(socket) = defaults.remove("socket") {
                options = options.socket(Some(socket));
            }
            return Ok(Self::Mysql(options.into()));
        }
        if env("TOOL_DATA_DIR").is_some() || env("TOOL_TOOLSDB_USER").is_some() {
            return Ok(Self::Unconfigured);
        }
        Ok(Self::Sqlite(
            sqlite
                .or_else(|| env("ORPHANWIKI_REVIEW_SQLITE").map(PathBuf::from))
                .map(|p| root.join(p))
                .unwrap_or_else(|| root.join("data/editorial.sqlite")),
        ))
    }

    pub(super) fn connect(&self) -> ReviewResult<Storage> {
        let mut storage = match self {
            Self::Sqlite(path) => {
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).map_err(|_| ReviewError::unavailable())?;
                }
                let connection =
                    rusqlite::Connection::open(path).map_err(|_| ReviewError::unavailable())?;
                connection
                    .busy_timeout(Duration::from_secs(10))
                    .map_err(|_| ReviewError::unavailable())?;
                Storage::Sqlite(connection)
            }
            Self::Mysql(options) => Storage::Mysql(
                mysql::Conn::new(options.clone()).map_err(|_| ReviewError::unavailable())?,
            ),
            Self::Unconfigured => {
                return Err(ReviewError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Configurare ToolsDB per le valutazioni su Toolforge",
                ))
            }
        };
        let schema = if matches!(storage, Storage::Mysql(_)) {
            SCHEMA.replace(
                "reviewer VARCHAR(255)",
                "reviewer VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin",
            )
        } else {
            SCHEMA.to_owned()
        };
        storage.execute(&schema, &[])?;
        Ok(storage)
    }
}

pub(super) fn client_defaults(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut client = false;
    let mut result = BTreeMap::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with(['#', ';']))
    {
        if line.starts_with('[') && line.ends_with(']') {
            client = line == "[client]";
        } else if client {
            let (key, value) = line
                .split_once('=')
                .ok_or("Configurazione ToolsDB non valida")?;
            let value = value.trim();
            let value = if value.starts_with(['\'', '"']) {
                if value.len() < 2 || value.as_bytes().first() != value.as_bytes().last() {
                    return Err("Configurazione ToolsDB non valida".into());
                }
                &value[1..value.len() - 1]
            } else {
                value
            };
            result.insert(key.trim().into(), value.into());
        }
    }
    Ok(result)
}

pub(super) enum SqlValue {
    Text(String),
    Number(u64),
}
impl From<&str> for SqlValue {
    fn from(value: &str) -> Self {
        Self::Text(value.into())
    }
}
impl From<u64> for SqlValue {
    fn from(value: u64) -> Self {
        Self::Number(value)
    }
}

pub(super) enum Storage {
    Sqlite(rusqlite::Connection),
    Mysql(mysql::Conn),
}

impl Storage {
    fn sqlite_parameters(values: &[SqlValue]) -> Vec<rusqlite::types::Value> {
        values
            .iter()
            .map(|v| match v {
                SqlValue::Text(v) => rusqlite::types::Value::Text(v.clone()),
                SqlValue::Number(v) => rusqlite::types::Value::Integer(*v as i64),
            })
            .collect()
    }

    fn mysql_parameters(values: &[SqlValue]) -> mysql::Params {
        mysql::Params::Positional(
            values
                .iter()
                .map(|v| match v {
                    SqlValue::Text(v) => mysql::Value::from(v.as_str()),
                    SqlValue::Number(v) => mysql::Value::from(*v),
                })
                .collect(),
        )
    }

    fn execute(&mut self, sql: &str, values: &[SqlValue]) -> ReviewResult<u64> {
        match self {
            Self::Sqlite(db) => db
                .execute(
                    sql,
                    rusqlite::params_from_iter(Self::sqlite_parameters(values)),
                )
                .map(|n| n as u64)
                .map_err(|_| ReviewError::unavailable()),
            Self::Mysql(db) => {
                db.exec_drop(sql, Self::mysql_parameters(values))
                    .map_err(|_| ReviewError::unavailable())?;
                Ok(db.affected_rows())
            }
        }
    }

    pub(super) fn read(&mut self, sql: &str, values: &[SqlValue]) -> ReviewResult<Vec<Review>> {
        match self {
            Self::Sqlite(db) => {
                let mut statement = db.prepare(sql).map_err(|_| ReviewError::unavailable())?;
                let rows = statement
                    .query_map(
                        rusqlite::params_from_iter(Self::sqlite_parameters(values)),
                        |row| {
                            Ok(Review {
                                language: row.get(0)?,
                                source_id: row.get(1)?,
                                target_id: row.get(2)?,
                                status: row.get(3)?,
                                version: row.get(4)?,
                                updated_at: row.get(5)?,
                                reviewer: row.get(6)?,
                            })
                        },
                    )
                    .map_err(|_| ReviewError::unavailable())?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|_| ReviewError::unavailable())
            }
            Self::Mysql(db) => {
                let rows: Vec<mysql::Row> = db
                    .exec(sql, Self::mysql_parameters(values))
                    .map_err(|_| ReviewError::unavailable())?;
                rows.into_iter()
                    .map(|row| {
                        let (language, source_id, target_id, status, version, updated_at, reviewer) =
                            mysql::from_row_opt(row).map_err(|_| ReviewError::unavailable())?;
                        Ok(Review {
                            language,
                            source_id,
                            target_id,
                            status,
                            version,
                            updated_at,
                            reviewer,
                        })
                    })
                    .collect()
            }
        }
    }

    pub(super) fn save(&mut self, body: &Submission, reviewer: &str) -> ReviewResult<Review> {
        let begin = if matches!(self, Self::Sqlite(_)) {
            "BEGIN IMMEDIATE"
        } else {
            "START TRANSACTION"
        };
        self.execute(begin, &[])?;
        let result = self.update(body, reviewer);
        match result {
            Ok(review) => {
                if let Err(error) = self.execute("COMMIT", &[]) {
                    let _ = self.execute("ROLLBACK", &[]);
                    return Err(error);
                }
                Ok(review)
            }
            Err(error) => {
                let _ = self.execute("ROLLBACK", &[]);
                Err(error)
            }
        }
    }

    fn update(&mut self, body: &Submission, reviewer: &str) -> ReviewResult<Review> {
        let now: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
        let now = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let identity = || {
            vec![
                body.language.as_str().into(),
                body.source_id.into(),
                body.target_id.into(),
            ]
        };
        let mut values = identity();
        values.extend(["not_fixed".into(), 0.into(), now.as_str().into(), "".into()]);
        let insert = if matches!(self, Self::Sqlite(_)) {
            "INSERT OR IGNORE"
        } else {
            "INSERT IGNORE"
        };
        self.execute(
            &format!("{insert} INTO editorial_reviews ({COLUMNS}) VALUES (?,?,?,?,?,?,?)"),
            &values,
        )?;
        let mut values = vec![
            body.status.as_str().into(),
            now.as_str().into(),
            reviewer.into(),
        ];
        values.extend(identity());
        values.push(body.version.into());
        let changed = self.execute("UPDATE editorial_reviews SET status=?, version=version+1, updated_at=?, reviewer=? WHERE language=? AND source_id=? AND target_id=? AND version=?", &values)?;
        let current = self.read(&format!("SELECT {COLUMNS} FROM editorial_reviews WHERE language=? AND source_id=? AND target_id=?"), &identity())?
            .pop().ok_or_else(ReviewError::unavailable)?;
        if changed == 0 {
            return Err(ReviewError { status: StatusCode::CONFLICT,
                message: "La coppia è stata valutata da un altro utente. Controlla lo stato aggiornato e riprova.", current: Some(Box::new(current)) });
        }
        Ok(current)
    }
}

// OAuth tokens are exchanged only on the server and discarded after identification.

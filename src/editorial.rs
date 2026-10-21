//! Shared human review state, isolated from immutable scientific exports.
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use mysql::prelude::Queryable;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::io::AsyncReadExt;

const MAX_ID: u64 = 9_007_199_254_740_991;
const COLUMNS: &str = "language, source_id, target_id, status, version, updated_at";
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS editorial_reviews (
    language VARCHAR(20) NOT NULL, source_id BIGINT NOT NULL, target_id BIGINT NOT NULL,
    status VARCHAR(12) NOT NULL, version BIGINT NOT NULL, updated_at VARCHAR(32) NOT NULL,
    PRIMARY KEY (language, target_id, source_id))";

#[derive(Debug)]
struct ReviewError {
    status: StatusCode,
    message: &'static str,
    current: Option<Review>,
}

impl ReviewError {
    fn new(status: StatusCode, message: &'static str) -> Self {
        Self {
            status,
            message,
            current: None,
        }
    }

    fn unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Servizio valutazioni non disponibile",
        )
    }

    fn invalid() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "Richiesta non valida")
    }

    fn missing_edge() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Coppia non presente nella raccolta approvata",
        )
    }
}

impl IntoResponse for ReviewError {
    fn into_response(self) -> Response {
        response(
            self.status,
            json!({"error": self.message, "current": self.current}),
        )
    }
}

type ReviewResult<T> = Result<T, ReviewError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Review {
    pub language: String,
    pub source_id: u64,
    pub target_id: u64,
    pub status: String,
    pub version: u64,
    pub updated_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submission {
    dataset: String,
    language: String,
    source_id: u64,
    target_id: u64,
    status: String,
    version: u64,
}

impl Submission {
    fn validate(&self) -> ReviewResult<()> {
        if !language(&self.language)
            || self.language == "all"
            || !valid_id(self.source_id)
            || !valid_id(self.target_id)
            || self.source_id == self.target_id
            || self.version >= MAX_ID
            || !matches!(self.status.as_str(), "fixed" | "not_fixed" | "bad_link")
        {
            return Err(ReviewError::invalid());
        }
        Ok(())
    }
}

fn valid_id(value: u64) -> bool {
    value > 0 && value <= MAX_ID
}

fn language(value: &str) -> bool {
    (1..=20).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn filename(value: &str) -> bool {
    value.ends_with(".json")
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

fn directory(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

/// Database selection is explicit; SQLite is never silently used on Toolforge.
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

    fn connect(&self) -> ReviewResult<Storage> {
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
        storage.execute(SCHEMA, &[])?;
        Ok(storage)
    }
}

fn client_defaults(text: &str) -> Result<BTreeMap<String, String>, String> {
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

enum SqlValue {
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

enum Storage {
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

    fn read(&mut self, sql: &str, values: &[SqlValue]) -> ReviewResult<Vec<Review>> {
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
                        let (language, source_id, target_id, status, version, updated_at) =
                            mysql::from_row_opt(row).map_err(|_| ReviewError::unavailable())?;
                        Ok(Review {
                            language,
                            source_id,
                            target_id,
                            status,
                            version,
                            updated_at,
                        })
                    })
                    .collect()
            }
        }
    }

    fn save(&mut self, body: &Submission) -> ReviewResult<Review> {
        let begin = if matches!(self, Self::Sqlite(_)) {
            "BEGIN IMMEDIATE"
        } else {
            "START TRANSACTION"
        };
        self.execute(begin, &[])?;
        let result = self.update(body);
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

    fn update(&mut self, body: &Submission) -> ReviewResult<Review> {
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
        values.extend(["not_fixed".into(), 0.into(), now.as_str().into()]);
        let insert = if matches!(self, Self::Sqlite(_)) {
            "INSERT OR IGNORE"
        } else {
            "INSERT IGNORE"
        };
        self.execute(
            &format!("{insert} INTO editorial_reviews ({COLUMNS}) VALUES (?,?,?,?,?,?)"),
            &values,
        )?;
        let mut values = vec![body.status.as_str().into(), now.as_str().into()];
        values.extend(identity());
        values.push(body.version.into());
        let changed = self.execute("UPDATE editorial_reviews SET status=?, version=version+1, updated_at=? WHERE language=? AND source_id=? AND target_id=? AND version=?", &values)?;
        let current = self.read(&format!("SELECT {COLUMNS} FROM editorial_reviews WHERE language=? AND source_id=? AND target_id=?"), &identity())?
            .pop().ok_or_else(ReviewError::unavailable)?;
        if changed == 0 {
            return Err(ReviewError { status: StatusCode::CONFLICT,
                message: "La coppia è stata valutata da un altro utente. Controlla lo stato aggiornato e riprova.", current: Some(current) });
        }
        Ok(current)
    }
}

#[derive(Clone)]
pub struct ReviewApplication {
    web_root: PathBuf,
    database: ReviewDatabase,
}

impl ReviewApplication {
    pub fn new(web_root: PathBuf, database: ReviewDatabase) -> Self {
        Self { web_root, database }
    }

    fn data_file(&self, relative: &Path, missing: ReviewError) -> ReviewResult<PathBuf> {
        let root = self
            .web_root
            .join("data")
            .canonicalize()
            .map_err(|_| ReviewError::unavailable())?;
        let file = root.join(relative).canonicalize().map_err(|_| missing)?;
        if !file.starts_with(root) {
            return Err(ReviewError::new(
                StatusCode::BAD_REQUEST,
                "Percorso non valido",
            ));
        }
        Ok(file)
    }

    fn data_json(&self, relative: &Path, missing: ReviewError) -> ReviewResult<Value> {
        let path = self.data_file(relative, missing)?;
        let bytes = std::fs::read(path).map_err(|_| ReviewError::unavailable())?;
        serde_json::from_slice(&bytes).map_err(|_| ReviewError::invalid())
    }

    fn members(&self, dataset: &str) -> ReviewResult<Vec<Value>> {
        if !filename(dataset) {
            return Err(ReviewError::new(
                StatusCode::BAD_REQUEST,
                "Raccolta non valida",
            ));
        }
        let entries = self.data_json(Path::new("manifest.json"), ReviewError::unavailable())?;
        let entries = entries.as_array().ok_or_else(ReviewError::invalid)?;
        let entry = entries
            .iter()
            .find(|row| row["file"] == dataset && row["demo"] != true)
            .ok_or_else(|| ReviewError::new(StatusCode::NOT_FOUND, "Raccolta non approvata"))?;
        if entry["kind"] != "aggregate" {
            return Ok(vec![entry.clone()]);
        }
        let index = self.data_json(Path::new(dataset), ReviewError::invalid())?;
        let members = index["members"]
            .as_array()
            .filter(|m| !m.is_empty())
            .ok_or_else(ReviewError::invalid)?;
        members
            .iter()
            .map(|member| {
                entries
                    .iter()
                    .find(|row| {
                        row["file"] == member["file"]
                            && row["language"] == member["language"]
                            && row["demo"] != true
                            && row["kind"] != "aggregate"
                    })
                    .cloned()
                    .ok_or_else(ReviewError::invalid)
            })
            .collect()
    }

    fn validate_edge(&self, body: &Submission) -> ReviewResult<()> {
        body.validate()?;
        let members = self.members(&body.dataset)?;
        let entry = members
            .iter()
            .find(|e| e["language"] == body.language)
            .ok_or_else(ReviewError::missing_edge)?;
        let folder = entry["connections_dir"]
            .as_str()
            .filter(|s| directory(s))
            .ok_or_else(ReviewError::missing_edge)?;
        let detail = self.data_json(
            &Path::new(folder).join(format!("{}.json", body.target_id)),
            ReviewError::missing_edge(),
        )?;
        let metadata = if entry.get("connections_metadata_file").is_some() {
            let name = entry["connections_metadata_file"]
                .as_str()
                .filter(|s| filename(s))
                .ok_or_else(ReviewError::missing_edge)?;
            let shared = self.data_json(Path::new(name), ReviewError::missing_edge())?;
            let identity = shared["connection_identity"].as_str().filter(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            });
            if detail["format"] != "connections_v2"
                || shared["format"] != "connections_metadata_v1"
                || identity.is_none()
                || shared["connection_identity"] != detail["connection_identity"]
                || !shared["census_page_count"].as_u64().is_some_and(valid_id)
                || !detail["census_page_count"].as_u64().is_some_and(valid_id)
                || shared["census_page_count"] != detail["census_page_count"]
                || detail.get("metadata").is_some()
            {
                return Err(ReviewError::missing_edge());
            }
            shared["metadata"].clone()
        } else {
            if detail["format"] != "connections_v1" {
                return Err(ReviewError::missing_edge());
            }
            detail["metadata"].clone()
        };
        let pages = detail["pages"]
            .as_array()
            .ok_or_else(ReviewError::missing_edge)?;
        let candidates =
            detail["link_candidates"][body.target_id.to_string()]["candidates"].as_array();
        if metadata["language"] != body.language
            || metadata["finished_at"] != entry["date"]
            || metadata["complete"] != true
            || metadata["demo"] == true
            || pages.len() != 1
            || pages[0]["page_id"].as_u64() != Some(body.target_id)
            || pages[0]["orphan"] != true
            || !candidates.is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["page_id"].as_u64() == Some(body.source_id))
            })
        {
            return Err(ReviewError::missing_edge());
        }
        Ok(())
    }

    fn reviews(&self, dataset: &str) -> ReviewResult<Vec<Review>> {
        let members = self.members(dataset)?;
        let languages = members
            .iter()
            .map(|m| {
                m["language"]
                    .as_str()
                    .filter(|s| language(s))
                    .ok_or_else(ReviewError::invalid)
            })
            .collect::<ReviewResult<BTreeSet<_>>>()?;
        let markers = vec!["?"; languages.len()].join(",");
        let values = languages
            .into_iter()
            .map(SqlValue::from)
            .collect::<Vec<_>>();
        self.database.connect()?.read(&format!("SELECT {COLUMNS} FROM editorial_reviews WHERE language IN ({markers}) ORDER BY language, target_id, source_id"), &values)
    }

    fn save(&self, body: Submission) -> ReviewResult<Review> {
        self.validate_edge(&body)?;
        self.database.connect()?.save(&body)
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/api/reviews", any(api))
            .fallback(static_file)
            .with_state(Arc::new(self))
    }
}

fn response(status: StatusCode, value: Value) -> Response {
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Json(value),
    )
        .into_response()
}

async fn api(State(app): State<Arc<ReviewApplication>>, request: Request) -> Response {
    match api_request(app, request).await {
        Ok(value) => response(StatusCode::OK, value),
        Err(error) => error.into_response(),
    }
}

async fn api_request(app: Arc<ReviewApplication>, request: Request) -> ReviewResult<Value> {
    match *request.method() {
        Method::GET => {
            let fields: Vec<(String, String)> = axum::extract::Query::try_from_uri(request.uri())
                .map_err(|_| ReviewError::invalid())?
                .0;
            if fields.len() != 1 || fields[0].0 != "dataset" || fields[0].1.is_empty() {
                return Err(ReviewError::invalid());
            }
            let dataset = fields[0].1.clone();
            let reviews = tokio::task::spawn_blocking(move || app.reviews(&dataset))
                .await
                .map_err(|_| ReviewError::unavailable())??;
            Ok(json!({"format": "editorial_reviews_v1", "reviews": reviews}))
        }
        Method::POST => {
            let headers = request.headers();
            let text = |name| {
                headers
                    .get(name)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("")
            };
            let origin = text("origin");
            let host = text("host");
            if (!origin.is_empty()
                && origin != format!("https://{host}")
                && origin != format!("http://{host}"))
                || matches!(text("sec-fetch-site"), "cross-site" | "same-site")
            {
                return Err(ReviewError::new(
                    StatusCode::FORBIDDEN,
                    "Origine non consentita",
                ));
            }
            if text("content-type").split(';').next() != Some("application/json") {
                return Err(ReviewError::new(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "JSON richiesto",
                ));
            }
            let length = text("content-length").parse::<usize>().unwrap_or(0);
            if !(1..=2048).contains(&length) {
                return Err(ReviewError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Dimensione della richiesta non valida",
                ));
            }
            let bytes = to_bytes(request.into_body(), 2048)
                .await
                .map_err(|_| ReviewError::invalid())?;
            if bytes.len() != length {
                return Err(ReviewError::invalid());
            }
            let body: Submission =
                serde_json::from_slice(&bytes).map_err(|_| ReviewError::invalid())?;
            let review = tokio::task::spawn_blocking(move || app.save(body))
                .await
                .map_err(|_| ReviewError::unavailable())??;
            Ok(json!({"review": review}))
        }
        _ => Err(ReviewError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "Metodo non consentito",
        )),
    }
}

async fn static_file(State(app): State<Arc<ReviewApplication>>, request: Request) -> Response {
    match serve_file(&app.web_root, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn serve_file(root: &Path, request: Request) -> ReviewResult<Response> {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return Err(ReviewError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "Metodo non consentito",
        ));
    }
    let missing = || ReviewError::new(StatusCode::NOT_FOUND, "File non disponibile");
    let relative = request.uri().path().trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    let allowed = matches!(
        relative,
        "index.html" | "creators.html" | "deorphanize.html" | "connections.html"
    ) || relative.starts_with("assets/")
        || relative.starts_with("data/");
    if !allowed
        || Path::new(relative).components().any(|part| match part {
            Component::Normal(value) => value.to_str().is_none_or(|s| s.starts_with('.')),
            _ => true,
        })
    {
        return Err(missing());
    }
    let root = tokio::fs::canonicalize(root).await.map_err(|_| missing())?;
    let path = tokio::fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| missing())?;
    if !path.starts_with(root) {
        return Err(missing());
    }
    let file = tokio::fs::File::open(&path).await.map_err(|_| missing())?;
    let metadata = file.metadata().await.map_err(|_| missing())?;
    if !metadata.is_file() {
        return Err(missing());
    }
    let mime = match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "html" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/vnd.microsoft.icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    let body = if *request.method() == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(futures_util::stream::try_unfold(
            file,
            |mut file| async move {
                let mut buffer = vec![0; 65536];
                let size = file.read(&mut buffer).await?;
                buffer.truncate(size);
                Ok::<_, std::io::Error>((size > 0).then_some((buffer, file)))
            },
        ))
    };
    Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, metadata.len())
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(body)
        .map_err(|_| ReviewError::unavailable())
}

/// Blocking CLI entry point; HTTP and file bodies are served asynchronously.
pub fn serve(
    app: ReviewApplication,
    bind: &str,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let listener = tokio::net::TcpListener::bind((bind, port)).await?;
            println!("Dashboard: http://{}", listener.local_addr()?);
            axum::serve(listener, app.router())
                .with_graceful_shutdown(shutdown())
                .await
        })?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture {
        root: PathBuf,
        app: Arc<ReviewApplication>,
        body: Value,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "orphanwiki-reviews-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            let web = root.join("web");
            std::fs::create_dir_all(web.join("data")).unwrap();
            std::fs::write(web.join("index.html"), "<h1>Dashboard</h1>").unwrap();
            let mut entries = Vec::new();
            for lang in ["vec", "lmo"] {
                let directory = web.join(format!("data/{lang}-connections"));
                std::fs::create_dir(&directory).unwrap();
                Self::write(
                    &directory.join("1.json"),
                    &json!({
                        "format":"connections_v1", "metadata":{"language":lang, "complete":true, "demo":false, "finished_at":"2026-10-01T00:00:00Z"},
                        "pages":[{"page_id":1,"orphan":true}],"link_candidates":{"1":{"candidates":[{"page_id":2},{"page_id":3}]}}
                    }),
                );
                entries.push(json!({"language":lang,"file":format!("{lang}.json"),"kind":"wiki","date":"2026-10-01T00:00:00Z","connections_dir":format!("{lang}-connections")}));
            }
            Self::write(&web.join("data/all.json"), &json!({"members":entries}));
            entries.push(json!({"language":"all","file":"all.json","kind":"aggregate"}));
            Self::write(&web.join("data/manifest.json"), &json!(entries));
            let app = Arc::new(ReviewApplication::new(
                web,
                ReviewDatabase::Sqlite(root.join("reviews.sqlite")),
            ));
            Self {
                root,
                app,
                body: json!({"dataset":"vec.json","language":"vec","source_id":2,"target_id":1,"status":"fixed","version":0}),
            }
        }

        fn write(path: &Path, value: &Value) {
            std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
        }

        fn request(
            &self,
            method: Method,
            uri: &str,
            body: Option<Value>,
            headers: &[(&str, &str)],
        ) -> (StatusCode, Value) {
            let body = body
                .map(|v| serde_json::to_vec(&v).unwrap())
                .unwrap_or_default();
            let mut request = Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "example.toolforge.org")
                .header("content-type", "application/json")
                .header("content-length", body.len());
            for (key, value) in headers {
                request = request.header(*key, *value);
            }
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let response = api(
                    State(self.app.clone()),
                    request.body(Body::from(body)).unwrap(),
                )
                .await;
                let status = response.status();
                assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
                assert_eq!(
                    response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                    "nosniff"
                );
                (
                    status,
                    serde_json::from_slice(
                        &to_bytes(response.into_body(), usize::MAX).await.unwrap(),
                    )
                    .unwrap(),
                )
            })
        }

        fn submit(&self, body: Value) -> (StatusCode, Value) {
            self.request(Method::POST, "/api/reviews", Some(body), &[])
        }

        fn shared(&self) -> (PathBuf, Value, PathBuf, Value) {
            let path = self.app.web_root.join("data/vec-connections/1.json");
            let mut detail: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let shared = json!({"format":"connections_metadata_v1","connection_identity":"a".repeat(64),"census_page_count":3,"metadata":detail.as_object_mut().unwrap().remove("metadata").unwrap()});
            detail["format"] = json!("connections_v2");
            detail["connection_identity"] = shared["connection_identity"].clone();
            detail["census_page_count"] = json!(3);
            let metadata_path = self.app.web_root.join("data/vec-metadata.json");
            Self::write(&path, &detail);
            Self::write(&metadata_path, &shared);
            let manifest = self.app.web_root.join("data/manifest.json");
            let mut entries: Value =
                serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
            entries[0]["connections_metadata_file"] = json!("vec-metadata.json");
            Self::write(&manifest, &entries);
            (path, detail, metadata_path, shared)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn persistence_reopening_language_scope_and_private_schema() {
        let f = Fixture::new();
        let read = || f.request(Method::GET, "/api/reviews?dataset=all.json", None, &[]);
        assert_eq!(read().1["reviews"], json!([]));
        let (status, first) = f.submit(f.body.clone());
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first["review"]["version"], 1);
        let conflict = f.submit(f.body.clone());
        assert_eq!(conflict.0, StatusCode::CONFLICT);
        assert_eq!(conflict.1["current"], first["review"]);
        for (version, status) in [(1, "bad_link"), (2, "not_fixed")] {
            let mut body = f.body.clone();
            body["version"] = json!(version);
            body["status"] = json!(status);
            assert_eq!(f.submit(body).0, StatusCode::OK);
        }
        assert_eq!(read().1["reviews"][0]["status"], "not_fixed");
        assert_eq!(
            f.request(Method::GET, "/api/reviews?dataset=lmo.json", None, &[])
                .1["reviews"],
            json!([])
        );
        let db = rusqlite::Connection::open(f.root.join("reviews.sqlite")).unwrap();
        let names = db
            .prepare("PRAGMA table_info(editorial_reviews)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(names, COLUMNS.split(", ").collect::<Vec<_>>());
        let mut invalid = f.body.clone();
        invalid["source_id"] = json!(3);
        invalid["version"] = json!(8);
        assert_eq!(f.submit(invalid).0, StatusCode::CONFLICT);
        assert_eq!(read().1["reviews"].as_array().unwrap().len(), 1); // Failed first update rolls back its insertion.
    }

    #[test]
    fn concurrent_writers_do_not_overwrite_each_other() {
        let f = Fixture::new();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let app = f.app.clone();
                let body = f.body.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    app.save(serde_json::from_value(body).unwrap())
                        .map_err(|e| e.status)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(StatusCode::CONFLICT)))
                .count(),
            1
        );
    }

    #[test]
    fn invalid_submissions_and_unapproved_pairs_never_create_database() {
        let f = Fixture::new();
        for (key, value) in [
            ("status", json!("anything")),
            ("source_id", json!(true)),
            ("source_id", json!(1)),
            ("source_id", json!(9007199254740992u64)),
            ("target_id", json!(0)),
            ("version", json!(-1)),
            ("version", json!(true)),
            ("version", json!(1.0)),
            ("language", json!("../vec")),
            ("dataset", json!("../vec.json")),
            ("dataset", json!("unapproved.json")),
            ("source_id", json!(999)),
            ("language", json!("en")),
            ("wikitext", json!("never persisted")),
        ] {
            let mut body = f.body.clone();
            body[key] = value;
            assert!(
                matches!(f.submit(body).0.as_u16(), 400 | 404 | 422),
                "{key}"
            );
        }
        assert!(!f.root.join("reviews.sqlite").exists());
    }

    #[test]
    fn shared_metadata_rejects_stale_identities_counts_and_embedded_metadata() {
        let f = Fixture::new();
        let (path, detail, metadata_path, shared) = f.shared();
        for (key, value) in [
            ("format", json!("wrong")),
            ("connection_identity", json!("b".repeat(64))),
            ("census_page_count", json!(4)),
            ("census_page_count", json!(true)),
            ("metadata", Value::Null),
        ] {
            let mut changed = shared.clone();
            changed[key] = value;
            Fixture::write(&metadata_path, &changed);
            assert_eq!(f.submit(f.body.clone()).0, StatusCode::UNPROCESSABLE_ENTITY);
        }
        Fixture::write(&metadata_path, &shared);
        for (key, value) in [
            ("connection_identity", json!("b".repeat(64))),
            ("census_page_count", json!(3.0)),
            ("metadata", shared["metadata"].clone()),
            ("format", json!("connections_v1")),
        ] {
            let mut changed = detail.clone();
            changed[key] = value;
            Fixture::write(&path, &changed);
            assert_eq!(f.submit(f.body.clone()).0, StatusCode::UNPROCESSABLE_ENTITY);
        }
        Fixture::write(&path, &detail);
        for (key, value) in [
            ("language", json!("en")),
            ("finished_at", json!("old")),
            ("complete", json!(false)),
            ("demo", json!(true)),
        ] {
            let mut changed = shared.clone();
            changed["metadata"][key] = value;
            Fixture::write(&metadata_path, &changed);
            assert_eq!(f.submit(f.body.clone()).0, StatusCode::UNPROCESSABLE_ENTITY);
        }
        Fixture::write(&metadata_path, &shared);
        assert_eq!(f.submit(f.body.clone()).0, StatusCode::OK);
        let mut body = f.body.clone();
        body["dataset"] = json!("all.json");
        body["language"] = json!("lmo");
        assert_eq!(f.submit(body).0, StatusCode::OK);
    }

    #[test]
    fn http_contract_blocks_cross_origin_forms_duplicate_queries_and_wrong_methods() {
        let f = Fixture::new();
        for headers in [
            vec![("origin", "https://other.example")],
            vec![("sec-fetch-site", "cross-site")],
            vec![("sec-fetch-site", "same-site")],
        ] {
            assert_eq!(
                f.request(Method::POST, "/api/reviews", Some(f.body.clone()), &headers)
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        for uri in [
            "/api/reviews",
            "/api/reviews?dataset=vec.json&dataset=lmo.json",
            "/api/reviews?dataset=vec.json&other=1",
        ] {
            assert_eq!(
                f.request(Method::GET, uri, None, &[]).0,
                StatusCode::BAD_REQUEST
            );
        }
        for method in [Method::DELETE, Method::HEAD] {
            assert_eq!(
                f.request(method, "/api/reviews", None, &[]).0,
                StatusCode::METHOD_NOT_ALLOWED
            );
        }
        let mut oversized = f.body.clone();
        oversized["text"] = json!("x".repeat(3000));
        assert_eq!(f.submit(oversized).0, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(!f.root.join("reviews.sqlite").exists());
    }

    #[test]
    fn static_files_stream_and_paths_cannot_escape_web_root() {
        let f = Fixture::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for method in [Method::GET, Method::HEAD] {
                let response = serve_file(
                    &f.app.web_root,
                    Request::builder()
                        .method(method.clone())
                        .uri("/")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
                assert_eq!(response.headers()[header::CONTENT_LENGTH], "18");
                let bytes = to_bytes(response.into_body(), 100).await.unwrap();
                assert_eq!(
                    bytes.as_ref(),
                    if method == Method::GET {
                        b"<h1>Dashboard</h1>".as_slice()
                    } else {
                        b""
                    }
                );
            }
            for path in [
                "/../secret",
                "/data/../../secret",
                "/.env",
                "/editorial.sqlite",
            ] {
                assert_eq!(
                    serve_file(
                        &f.app.web_root,
                        Request::builder().uri(path).body(Body::empty()).unwrap()
                    )
                    .await
                    .unwrap_err()
                    .status,
                    StatusCode::NOT_FOUND
                );
            }
            #[cfg(unix)]
            {
                std::fs::write(f.root.join("secret"), "private").unwrap();
                std::os::unix::fs::symlink(
                    f.root.join("secret"),
                    f.app.web_root.join("data/outside"),
                )
                .unwrap();
                assert_eq!(
                    serve_file(
                        &f.app.web_root,
                        Request::builder()
                            .uri("/data/outside")
                            .body(Body::empty())
                            .unwrap()
                    )
                    .await
                    .unwrap_err()
                    .status,
                    StatusCode::NOT_FOUND
                );
            }
        });
    }

    #[test]
    fn mariadb_persistence_conflicts_and_rollback_on_isolated_test_database() {
        let Ok(url) = std::env::var("ORPHANWIKI_TEST_MYSQL_URL") else {
            return;
        };
        let options = mysql::Opts::from_url(&url).expect("Invalid isolated test database URL");
        assert_eq!(options.get_db_name(), Some("orphanwiki_test"));
        assert!(matches!(
            options.get_ip_or_hostname().as_ref(),
            "127.0.0.1" | "localhost"
        ));
        let mut f = Fixture::new();
        Arc::get_mut(&mut f.app).unwrap().database = ReviewDatabase::Mysql(options.clone());
        let mut connection = mysql::Conn::new(options).unwrap();
        connection
            .query_drop("DROP TABLE IF EXISTS editorial_reviews")
            .unwrap();
        assert_eq!(f.submit(f.body.clone()).0, StatusCode::OK);
        let conflict = f.submit(f.body.clone());
        assert_eq!(conflict.0, StatusCode::CONFLICT);
        assert_eq!(conflict.1["current"]["status"], "fixed");
        let mut stale = f.body.clone();
        stale["source_id"] = json!(3);
        stale["version"] = json!(8);
        assert_eq!(f.submit(stale).0, StatusCode::CONFLICT);
        let (status, read) = f.request(Method::GET, "/api/reviews?dataset=all.json", None, &[]);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read["reviews"].as_array().unwrap().len(), 1);
        let columns: Vec<mysql::Row> = connection
            .query("SHOW COLUMNS FROM editorial_reviews")
            .unwrap();
        assert_eq!(
            columns
                .into_iter()
                .map(|r| r.get::<String, _>(0).unwrap())
                .collect::<Vec<_>>(),
            COLUMNS.split(", ").collect::<Vec<_>>()
        );
        connection
            .query_drop("DROP TABLE editorial_reviews")
            .unwrap();
    }

    #[test]
    fn toolsdb_defaults_and_missing_configuration_are_explicit() {
        let settings = client_defaults(
            "[other]\nuser=ignored\n[client]\nuser=s123\npassword='synthetic secret'\nport=3307\n",
        )
        .unwrap();
        assert_eq!(settings["user"], "s123");
        assert_eq!(settings["password"], "synthetic secret");
        assert!(client_defaults("[client]\npassword='unclosed").is_err());
        assert!(matches!(
            ReviewDatabase::Unconfigured.connect(),
            Err(ReviewError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                ..
            })
        ));
    }
}

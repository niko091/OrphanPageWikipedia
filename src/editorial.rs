//! Shared human review state, isolated from immutable scientific exports.
mod auth;
mod http;
mod reviews;
mod storage;
use auth::{auth, LoginSession, OAuthConfig};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use http::{api, export_csv, page_query, response, static_file};
use mysql::prelude::Queryable;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
pub use storage::ReviewDatabase;
use storage::SqlValue;
use tokio::io::AsyncReadExt;

const MAX_ID: u64 = 9_007_199_254_740_991;
const COLUMNS: &str = "language, source_id, target_id, status, version, updated_at, reviewer";
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS editorial_reviews (
    language VARCHAR(20) NOT NULL, source_id BIGINT NOT NULL, target_id BIGINT NOT NULL,
    status VARCHAR(12) NOT NULL, version BIGINT NOT NULL, updated_at VARCHAR(32) NOT NULL,
    reviewer VARCHAR(255) NOT NULL DEFAULT '',
    PRIMARY KEY (language, target_id, source_id))";

#[derive(Debug)]
struct ReviewError {
    status: StatusCode,
    message: &'static str,
    current: Option<Box<Review>>,
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
    /// Verified Wikimedia identity; empty for historical anonymous reviews.
    #[serde(default)]
    pub reviewer: String,
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
pub struct ReviewApplication {
    web_root: PathBuf,
    collection_database: PathBuf,
    database: ReviewDatabase,
    oauth: Option<OAuthConfig>,
    sessions: Arc<Mutex<BTreeMap<String, LoginSession>>>,
}

impl ReviewApplication {
    pub fn new(web_root: PathBuf, database: ReviewDatabase) -> Self {
        Self {
            collection_database: web_root.join("data/collection.sqlite"),
            web_root,
            database,
            oauth: None,
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn with_collection_database(mut self, path: PathBuf) -> Self {
        self.collection_database = path;
        self
    }

    /// OAuth is optional for local anonymous review installations; partial configuration is an error.
    pub fn with_oauth_from_environment(mut self) -> Result<Self, String> {
        let values = [
            "ORPHANWIKI_OAUTH_CLIENT_ID",
            "ORPHANWIKI_OAUTH_CLIENT_SECRET",
            "ORPHANWIKI_OAUTH_REDIRECT_URI",
        ]
        .map(|key| std::env::var(key).unwrap_or_default());
        if values.iter().all(String::is_empty) {
            return Ok(self);
        }
        self.oauth = Some(OAuthConfig::parse(
            values[0].clone(),
            values[1].clone(),
            &values[2],
        )?);
        Ok(self)
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/api/reviews", any(api))
            .route("/api/pages", any(page_query))
            .route("/api/export", any(export_csv))
            .route("/api/auth/{action}", any(auth))
            .fallback(static_file)
            .with_state(Arc::new(self))
    }
}

/// Blocking CLI entry point; HTTP and file bodies are served asynchronously.
pub fn serve(
    app: ReviewApplication,
    bind: &str,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    crate::store::Census::open(&app.collection_database)?.verify()?;
    if let ReviewDatabase::Sqlite(path) = &app.database {
        if crate::dumps::resolved_path(path)?
            == crate::dumps::resolved_path(&app.collection_database)?
        {
            return Err(
                "Il database editoriale deve essere separato dalla raccolta scientifica".into(),
            );
        }
    }
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
mod tests;

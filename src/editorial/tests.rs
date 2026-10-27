use super::auth::{random_key, verified_username};
use super::http::serve_file;
use super::storage::client_defaults;
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
                    "format":"connections_v2", "connection_identity":"a".repeat(64),"census_page_count":3,
                    "pages":[{"page_id":1,"orphan":true}],"link_candidates":{"1":{"candidates":[{"page_id":2},{"page_id":3}]}}
                }),
            );
            Self::write(
                &web.join(format!("data/{lang}-metadata.json")),
                &json!({"format":"connections_metadata_v1","connection_identity":"a".repeat(64),"census_page_count":3,"metadata":{"language":lang,"complete":true,"demo":false,"finished_at":"2026-10-01T00:00:00Z"}}),
            );
            entries.push(json!({"connections_metadata_file":format!("{lang}-metadata.json"),"language":lang,"file":format!("{lang}.json"),"kind":"wiki","date":"2026-10-01T00:00:00Z","connections_dir":format!("{lang}-connections")}));
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
        let data_root = path
            .ancestors()
            .find(|p| p.file_name().is_some_and(|n| n == "data"))
            .unwrap();
        let db = rusqlite::Connection::open(data_root.join("collection.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE IF NOT EXISTS publication(key TEXT PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS documents(path TEXT PRIMARY KEY,data TEXT NOT NULL);").unwrap();
        db.execute(
            "INSERT OR IGNORE INTO publication VALUES('rule',?)",
            [crate::store::RULE],
        )
        .unwrap();
        db.execute(
            "INSERT OR REPLACE INTO documents VALUES(?,?)",
            rusqlite::params![
                path.strip_prefix(data_root).unwrap().to_str().unwrap(),
                serde_json::to_string(value).unwrap()
            ],
        )
        .unwrap();
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
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap(),
            )
        })
    }

    fn submit(&self, body: Value) -> (StatusCode, Value) {
        self.request(Method::POST, "/api/reviews", Some(body), &[])
    }

    fn shared(&self) -> (PathBuf, Value, PathBuf, Value) {
        let path = self.app.web_root.join("data/vec-connections/1.json");
        let detail: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let metadata_path = self.app.web_root.join("data/vec-metadata.json");
        let shared: Value =
            serde_json::from_slice(&std::fs::read(&metadata_path).unwrap()).unwrap();
        (path, detail, metadata_path, shared)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn enable_oauth(f: &mut Fixture) {
    Arc::get_mut(&mut f.app).unwrap().oauth = Some(
        OAuthConfig::parse(
            "test-client".into(),
            "test-secret".into(),
            "https://example.toolforge.org/api/auth/callback",
        )
        .unwrap(),
    );
}

fn auth_call(f: &Fixture, method: Method, uri: &str, headers: &[(&str, &str)]) -> Response {
    let mut request = Request::builder().method(method).uri(uri);
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { auth(State(f.app.clone()), request.body(Body::empty()).unwrap()).await })
}

#[test]
fn oauth_authorization_binds_browser_state_and_blocks_replay_and_redirects() {
    let mut f = Fixture::new();
    enable_oauth(&mut f);
    for uri in [
        "/api/auth/login?return_to=https://evil.invalid",
        "/api/auth/login?return_to=//evil.invalid",
        "/api/auth/login?return_to=connections.html%0d%0aLocation:evil",
        "/api/auth/login?return_to=index.html&return_to=creators.html",
    ] {
        assert_eq!(
            auth_call(&f, Method::GET, uri, &[]).status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        auth_call(
            &f,
            Method::GET,
            "/api/auth/login",
            &[("sec-fetch-site", "cross-site")]
        )
        .status(),
        StatusCode::FORBIDDEN
    );
    let response = auth_call(
        &f,
        Method::GET,
        "/api/auth/login?return_to=connections.html%3Fdataset%3Dvec.json%26page%3D1",
        &[],
    );
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("HttpOnly; SameSite=Lax; Max-Age=600; Secure"));
    let browser_cookie = cookie.split(';').next().unwrap();
    let location =
        reqwest::Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(location.host_str(), Some("meta.wikimedia.org"));
    let params = location.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(params["client_id"], "test-client");
    assert_eq!(params["response_type"], "code");
    assert!(!location.as_str().contains("test-secret"));
    let state = &params["state"];
    let callback = format!("/api/auth/callback?state={state}&error=access_denied");
    assert_eq!(
        auth_call(&f, Method::GET, &callback, &[]).status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        auth_call(
            &f,
            Method::GET,
            "/api/auth/callback?state=wrong&code=unused",
            &[("cookie", browser_cookie)]
        )
        .status(),
        StatusCode::BAD_REQUEST
    );
    let response = auth_call(&f, Method::GET, &callback, &[("cookie", browser_cookie)]);
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()[header::LOCATION],
        "https://example.toolforge.org/connections.html?dataset=vec.json&page=1"
    );
    assert!(response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    assert_eq!(
        auth_call(&f, Method::GET, &callback, &[("cookie", browser_cookie)]).status(),
        StatusCode::BAD_REQUEST
    );
    assert!(f.app.sessions.lock().unwrap().is_empty());
}

#[test]
fn authenticated_reviews_cannot_forge_authors_and_logout_revokes_writes() {
    let mut f = Fixture::new();
    enable_oauth(&mut f);
    assert_eq!(f.submit(f.body.clone()).0, StatusCode::UNAUTHORIZED);
    let key = random_key().unwrap();
    f.app.sessions.lock().unwrap().insert(
        key.clone(),
        LoginSession {
            expires: std::time::Instant::now() + Duration::from_secs(60),
            username: Some("Editor À & Test".into()),
            state: String::new(),
            return_to: String::new(),
        },
    );
    let cookie = format!("orphanwiki_session={key}");
    let headers = [
        ("cookie", cookie.as_str()),
        ("origin", "https://example.toolforge.org"),
    ];
    let mut forged = f.body.clone();
    forged["reviewer"] = json!("Impersonated");
    assert_eq!(
        f.request(Method::POST, "/api/reviews", Some(forged), &headers)
            .0,
        StatusCode::BAD_REQUEST
    );
    let saved = f.request(Method::POST, "/api/reviews", Some(f.body.clone()), &headers);
    assert_eq!(saved.0, StatusCode::OK);
    assert_eq!(saved.1["review"]["reviewer"], "Editor À & Test");
    assert_eq!(
        f.request(Method::GET, "/api/reviews?dataset=vec.json", None, &[])
            .1["reviews"][0]["reviewer"],
        "Editor À & Test"
    );
    let conflict = f.request(Method::POST, "/api/reviews", Some(f.body.clone()), &headers);
    assert_eq!(conflict.0, StatusCode::CONFLICT);
    assert_eq!(conflict.1["current"], saved.1["review"]);
    assert_eq!(
        auth_call(
            &f,
            Method::POST,
            "/api/auth/logout",
            &[
                ("cookie", &cookie),
                ("origin", "https://evil.invalid"),
                ("content-type", "application/json")
            ]
        )
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        auth_call(&f, Method::GET, "/api/auth/logout", &[("cookie", &cookie)]).status(),
        StatusCode::NOT_FOUND
    );
    assert!(f.app.sessions.lock().unwrap().contains_key(&key));
    assert_eq!(
        auth_call(
            &f,
            Method::POST,
            "/api/auth/logout",
            &[
                ("cookie", &cookie),
                ("origin", "https://example.toolforge.org"),
                ("content-type", "application/json")
            ]
        )
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        f.request(Method::POST, "/api/reviews", Some(f.body.clone()), &headers)
            .0,
        StatusCode::UNAUTHORIZED
    );
    f.app.sessions.lock().unwrap().insert(
        key,
        LoginSession {
            expires: std::time::Instant::now() - Duration::from_secs(1),
            username: Some("Expired".into()),
            state: String::new(),
            return_to: String::new(),
        },
    );
    assert_eq!(
        f.request(Method::POST, "/api/reviews", Some(f.body.clone()), &headers)
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[test]
fn oauth_server_exchange_uses_form_secret_and_bearer_profile_only() {
    use std::net::TcpListener;
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        for step in 0..2 {
            let (mut stream, _) = server.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut lines = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                lines.push(line);
            }
            let headers = lines.join("").to_lowercase();
            if step == 0 {
                assert_eq!(lines[0], "POST /token HTTP/1.1\r\n");
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                assert!(body.contains("client_secret=test-secret"));
                assert!(body.contains("code=code%26value"));
            } else {
                assert_eq!(lines[0], "GET /profile HTTP/1.1\r\n");
                assert!(headers.contains("authorization: bearer synthetic-token"));
            }
            let body = if step == 0 {
                r#"{"access_token":"synthetic-token","refresh_token":"discard"}"#
            } else {
                r#"{"username":"Editor À & Test","sub":"123","email":"discard"}"#
            };
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        }
    });
    let config = OAuthConfig::parse(
        "test-client".into(),
        "test-secret".into(),
        "https://example.org/api/auth/callback",
    )
    .unwrap();
    assert_eq!(
        config
            .identify_at(
                "code&value",
                &format!("http://{address}/token"),
                &format!("http://{address}/profile")
            )
            .unwrap(),
        "Editor À & Test"
    );
    worker.join().unwrap();
    for value in [
        json!({}),
        json!({"username":"Bad#name"}),
        json!({"username":""}),
        json!({"username":"Bad\nname"}),
    ] {
        assert!(verified_username(&value).is_err());
    }
    for callback in [
        "http://example.org/api/auth/callback",
        "https://user:secret@example.org/api/auth/callback",
        "https://example.org/api/auth/callback?secret=x",
    ] {
        assert!(OAuthConfig::parse("id".into(), "secret".into(), callback).is_err());
    }
    let local = OAuthConfig::parse(
        "id".into(),
        "secret".into(),
        "http://localhost:8000/tool/api/auth/callback",
    )
    .unwrap();
    assert_eq!(
        local.cookie("abc", 10),
        "orphanwiki_session=abc; Path=/tool/; HttpOnly; SameSite=Lax; Max-Age=10"
    );
}

#[test]
fn obsolete_review_schema_errors_without_altering_existing_rows() {
    let f = Fixture::new();
    let db = rusqlite::Connection::open(f.root.join("reviews.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE editorial_reviews (language TEXT,source_id INTEGER,target_id INTEGER,status TEXT,version INTEGER,updated_at TEXT,PRIMARY KEY(language,target_id,source_id));
        INSERT INTO editorial_reviews VALUES ('vec',2,1,'fixed',4,'2026-10-01T00:00:00Z');").unwrap();
    let result = f.request(Method::GET, "/api/reviews?dataset=vec.json", None, &[]);
    assert_eq!(result.0, StatusCode::SERVICE_UNAVAILABLE);
    let stored: (u64, String) = db
        .query_row("SELECT version,status FROM editorial_reviews", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(stored, (4, "fixed".into()));
    assert!(db
        .prepare("SELECT reviewer FROM editorial_reviews")
        .is_err());
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
                app.save(serde_json::from_value(body).unwrap(), "")
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
            std::os::unix::fs::symlink(f.root.join("secret"), f.app.web_root.join("data/outside"))
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

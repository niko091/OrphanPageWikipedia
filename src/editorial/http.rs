use super::*;

pub(super) async fn page_query(
    State(app): State<Arc<ReviewApplication>>,
    request: Request,
) -> Response {
    if request.method() != Method::POST {
        return ReviewError::new(StatusCode::METHOD_NOT_ALLOWED, "Metodo non consentito")
            .into_response();
    }
    let body = match to_bytes(request.into_body(), 8 * 1024 * 1024).await {
        Ok(body) => body,
        Err(_) => return ReviewError::invalid().into_response(),
    };
    let query: crate::store::PageQuery = match serde_json::from_slice(&body) {
        Ok(query) => query,
        Err(_) => return ReviewError::invalid().into_response(),
    };
    match tokio::task::spawn_blocking(move || {
        // Ordering uses the review snapshot explicitly loaded by this browser;
        // filtering must not silently refresh shared workflow state.
        crate::store::Census::open(&app.collection_database)
            .and_then(|db| db.query(&query, &query.closed_pairs))
    })
    .await
    {
        Ok(Ok(value)) => response(StatusCode::OK, value),
        _ => ReviewError::invalid().into_response(),
    }
}

pub(super) fn response(status: StatusCode, value: Value) -> Response {
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

pub(super) async fn api(State(app): State<Arc<ReviewApplication>>, request: Request) -> Response {
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
            let allowed = app
                .oauth
                .as_ref()
                .map(|config| config.redirect_uri.origin().ascii_serialization());
            if (!origin.is_empty()
                && if let Some(allowed) = allowed {
                    origin != allowed
                } else {
                    origin != format!("https://{host}") && origin != format!("http://{host}")
                })
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
            let reviewer = app.reviewer(&request)?;
            let bytes = to_bytes(request.into_body(), 2048)
                .await
                .map_err(|_| ReviewError::invalid())?;
            if bytes.len() != length {
                return Err(ReviewError::invalid());
            }
            let body: Submission =
                serde_json::from_slice(&bytes).map_err(|_| ReviewError::invalid())?;
            let review = tokio::task::spawn_blocking(move || app.save(body, &reviewer))
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

pub(super) async fn static_file(
    State(app): State<Arc<ReviewApplication>>,
    request: Request,
) -> Response {
    if request.uri().path().starts_with("/data/") {
        if !matches!(*request.method(), Method::GET | Method::HEAD) {
            return ReviewError::new(StatusCode::METHOD_NOT_ALLOWED, "Metodo non consentito")
                .into_response();
        }
        let path = request.uri().path().trim_start_matches("/data/").to_owned();
        let head = request.method() == Method::HEAD;
        return match tokio::task::spawn_blocking(move || {
            app.data_json(
                Path::new(&path),
                ReviewError::new(StatusCode::NOT_FOUND, "Risorsa non disponibile"),
            )
        })
        .await
        {
            Ok(Ok(value)) => {
                let mut result = response(StatusCode::OK, value);
                if head {
                    *result.body_mut() = Body::empty();
                }
                result
            }
            Ok(Err(error)) => error.into_response(),
            Err(_) => ReviewError::unavailable().into_response(),
        };
    }
    match serve_file(&app.web_root, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

pub(super) async fn serve_file(root: &Path, request: Request) -> ReviewResult<Response> {
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
        "index.html" | "creators.html" | "deorphanize.html" | "connections.html" | "about.html"
    ) || relative.starts_with("assets/");
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

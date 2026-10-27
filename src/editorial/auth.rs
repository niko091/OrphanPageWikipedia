use super::*;

#[derive(Clone)]
pub(super) struct OAuthConfig {
    pub(super) client_id: String,
    pub(super) client_secret: String,
    pub(super) redirect_uri: reqwest::Url,
}

impl OAuthConfig {
    pub(super) fn parse(
        client_id: String,
        client_secret: String,
        redirect: &str,
    ) -> Result<Self, String> {
        let redirect_uri =
            reqwest::Url::parse(redirect).map_err(|_| "Callback OAuth non valido")?;
        let local = matches!(
            redirect_uri.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        );
        if client_id.is_empty()
            || client_secret.is_empty()
            || !(redirect_uri.scheme() == "https" || local && redirect_uri.scheme() == "http")
            || redirect_uri.host_str().is_none()
            || !redirect_uri.username().is_empty()
            || redirect_uri.password().is_some()
            || redirect_uri.query().is_some()
            || redirect_uri.fragment().is_some()
            || !redirect_uri.path().ends_with("/api/auth/callback")
        {
            return Err("Configurazione OAuth non valida: usare un callback HTTPS /api/auth/callback (HTTP solo su localhost)".into());
        }
        Ok(Self {
            client_id,
            client_secret,
            redirect_uri,
        })
    }

    pub(super) fn cookie(&self, value: &str, seconds: u64) -> String {
        let path = self
            .redirect_uri
            .path()
            .strip_suffix("api/auth/callback")
            .unwrap_or("/");
        format!(
            "orphanwiki_session={value}; Path={path}; HttpOnly; SameSite=Lax; Max-Age={seconds}{}",
            if self.redirect_uri.scheme() == "https" {
                "; Secure"
            } else {
                ""
            }
        )
    }

    fn identify(&self, code: &str) -> ReviewResult<String> {
        self.identify_at(
            code,
            "https://meta.wikimedia.org/w/rest.php/oauth2/access_token",
            "https://meta.wikimedia.org/w/rest.php/oauth2/resource/profile",
        )
    }

    pub(super) fn identify_at(
        &self,
        code: &str,
        token_url: &str,
        profile_url: &str,
    ) -> ReviewResult<String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("OrphanWiki/0.1 (Wikimedia OAuth identity)")
            .build()
            .map_err(|_| ReviewError::unavailable())?;
        let token = client
            .post(token_url)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("redirect_uri", self.redirect_uri.as_str()),
            ])
            .send()
            .map_err(|_| ReviewError::unavailable())?;
        let token = oauth_json(token)?;
        let token = token["access_token"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(ReviewError::unavailable)?;
        let profile = client
            .get(profile_url)
            .bearer_auth(token)
            .send()
            .map_err(|_| ReviewError::unavailable())?;
        verified_username(&oauth_json(profile)?)
    }
}

pub(super) fn oauth_json(response: reqwest::blocking::Response) -> ReviewResult<Value> {
    if !response.status().is_success() {
        return Err(ReviewError::unavailable());
    }
    let mut bytes = Vec::new();
    response
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| ReviewError::unavailable())?;
    if bytes.len() > 65536 {
        return Err(ReviewError::unavailable());
    }
    serde_json::from_slice(&bytes).map_err(|_| ReviewError::unavailable())
}

pub(super) fn verified_username(profile: &Value) -> ReviewResult<String> {
    profile["username"]
        .as_str()
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 255
                && !name
                    .chars()
                    .any(|c| c.is_control() || "#<>|[]{}".contains(c))
        })
        .map(str::to_owned)
        .ok_or_else(ReviewError::unavailable)
}

pub(super) struct LoginSession {
    pub(super) expires: std::time::Instant,
    pub(super) username: Option<String>,
    pub(super) state: String,
    pub(super) return_to: String,
}

pub(super) fn random_key() -> ReviewResult<String> {
    let mut bytes = [0_u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| ReviewError::unavailable())?;
    use std::fmt::Write;
    let mut value = String::with_capacity(64);
    for byte in bytes {
        write!(value, "{byte:02x}").map_err(|_| ReviewError::unavailable())?;
    }
    Ok(value)
}

pub(super) fn session_cookie(request: &Request) -> Option<&str> {
    let cookies = request.headers().get(header::COOKIE)?.to_str().ok()?;
    let mut matches = cookies
        .split(';')
        .filter_map(|part| part.trim().strip_prefix("orphanwiki_session="));
    let key = matches.next()?;
    if matches.next().is_some() || key.len() != 64 || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(key)
}

pub(super) fn return_page(value: &str) -> bool {
    value.len() <= 2048
        && !value
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == '#')
        && matches!(
            value.split('?').next(),
            Some("index.html" | "creators.html" | "deorphanize.html" | "connections.html")
        )
}

impl ReviewApplication {
    fn session(&self, request: &Request) -> ReviewResult<Option<String>> {
        let Some(key) = session_cookie(request) else {
            return Ok(None);
        };
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| ReviewError::unavailable())?;
        sessions.retain(|_, session| session.expires > std::time::Instant::now());
        Ok(sessions
            .get(key)
            .and_then(|session| session.username.clone()))
    }
    pub(super) fn reviewer(&self, request: &Request) -> ReviewResult<String> {
        if self.oauth.is_none() {
            return Ok(String::new());
        }
        self.session(request)?.ok_or_else(|| {
            ReviewError::new(
                StatusCode::UNAUTHORIZED,
                "Accedi con Wikipedia per salvare la valutazione",
            )
        })
    }
}
pub(super) async fn auth(State(app): State<Arc<ReviewApplication>>, request: Request) -> Response {
    match auth_request(app, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

fn auth_redirect(
    config: &OAuthConfig,
    location: &str,
    cookie: Option<(&str, u64)>,
) -> ReviewResult<Response> {
    let mut builder = axum::http::Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, location)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::REFERRER_POLICY, "no-referrer");
    if let Some((value, seconds)) = cookie {
        builder = builder.header(header::SET_COOKIE, config.cookie(value, seconds));
    }
    builder
        .body(Body::empty())
        .map_err(|_| ReviewError::unavailable())
}

async fn auth_request(app: Arc<ReviewApplication>, request: Request) -> ReviewResult<Response> {
    let action = request.uri().path().rsplit('/').next().unwrap_or("");
    if action == "session" && request.method() == Method::GET {
        return Ok(response(
            StatusCode::OK,
            json!({"enabled": app.oauth.is_some(), "username": app.session(&request)?}),
        ));
    }
    let config = app.oauth.as_ref().ok_or_else(|| {
        ReviewError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Accesso Wikipedia non configurato",
        )
    })?;
    if action == "logout" && request.method() == Method::POST {
        let origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok());
        let site = request
            .headers()
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok());
        if origin.is_some_and(|v| v != config.redirect_uri.origin().ascii_serialization())
            || matches!(site, Some("cross-site" | "same-site"))
            || request
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                != Some("application/json")
        {
            return Err(ReviewError::new(
                StatusCode::FORBIDDEN,
                "Origine non consentita",
            ));
        }
        if let Some(key) = session_cookie(&request) {
            app.sessions
                .lock()
                .map_err(|_| ReviewError::unavailable())?
                .remove(key);
        }
        let mut result = response(StatusCode::OK, json!({"enabled": true, "username": null}));
        result.headers_mut().insert(
            header::SET_COOKIE,
            config
                .cookie("", 0)
                .parse()
                .map_err(|_| ReviewError::unavailable())?,
        );
        return Ok(result);
    }
    if request.method() != Method::GET {
        return Err(ReviewError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "Metodo non consentito",
        ));
    }
    let fields: Vec<(String, String)> = axum::extract::Query::try_from_uri(request.uri())
        .map_err(|_| ReviewError::invalid())?
        .0;
    if action == "login" {
        if matches!(
            request
                .headers()
                .get("sec-fetch-site")
                .and_then(|v| v.to_str().ok()),
            Some("cross-site" | "same-site")
        ) {
            return Err(ReviewError::new(
                StatusCode::FORBIDDEN,
                "Origine non consentita",
            ));
        }
        if fields.len() > 1
            || fields
                .first()
                .is_some_and(|(key, value)| key != "return_to" || !return_page(value))
        {
            return Err(ReviewError::invalid());
        }
        let key = random_key()?;
        let state = random_key()?;
        let mut url = reqwest::Url::parse("https://meta.wikimedia.org/w/rest.php/oauth2/authorize")
            .map_err(|_| ReviewError::unavailable())?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", config.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", config.redirect_uri.as_str()),
            ("state", &state),
        ]);
        let mut sessions = app
            .sessions
            .lock()
            .map_err(|_| ReviewError::unavailable())?;
        sessions.retain(|_, session| session.expires > std::time::Instant::now());
        if sessions.len() >= 4096 {
            return Err(ReviewError::unavailable());
        }
        if let Some(old) = session_cookie(&request) {
            sessions.remove(old);
        }
        sessions.insert(
            key.clone(),
            LoginSession {
                expires: std::time::Instant::now() + Duration::from_secs(600),
                username: None,
                state,
                return_to: fields
                    .first()
                    .map(|(_, value)| value.clone())
                    .unwrap_or_else(|| "deorphanize.html".into()),
            },
        );
        return auth_redirect(config, url.as_str(), Some((&key, 600)));
    }
    if action == "callback" {
        if fields.len() > 4
            || fields
                .iter()
                .enumerate()
                .any(|(i, (key, _))| fields[..i].iter().any(|(other, _)| key == other))
        {
            return Err(ReviewError::invalid());
        }
        let state = fields
            .iter()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.as_str())
            .ok_or_else(ReviewError::invalid)?;
        let cookie = session_cookie(&request).ok_or_else(ReviewError::invalid)?;
        let pending = {
            let mut sessions = app
                .sessions
                .lock()
                .map_err(|_| ReviewError::unavailable())?;
            let session = sessions
                .get(cookie)
                .filter(|session| {
                    session.username.is_none()
                        && session.state == state
                        && session.expires > std::time::Instant::now()
                })
                .ok_or_else(ReviewError::invalid)?;
            let return_to = session.return_to.clone();
            sessions.remove(cookie);
            return_to
        };
        let base = config
            .redirect_uri
            .as_str()
            .strip_suffix("api/auth/callback")
            .ok_or_else(ReviewError::invalid)?;
        if fields.iter().any(|(key, _)| key == "error") {
            return auth_redirect(config, &format!("{base}{pending}"), Some(("", 0)));
        }
        let code = fields
            .iter()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.clone())
            .filter(|code| !code.is_empty() && code.len() <= 4096)
            .ok_or_else(ReviewError::invalid)?;
        let configuration = config.clone();
        let username = tokio::task::spawn_blocking(move || configuration.identify(&code))
            .await
            .map_err(|_| ReviewError::unavailable())??;
        let key = random_key()?;
        app.sessions
            .lock()
            .map_err(|_| ReviewError::unavailable())?
            .insert(
                key.clone(),
                LoginSession {
                    expires: std::time::Instant::now() + Duration::from_secs(28800),
                    username: Some(username),
                    state: String::new(),
                    return_to: String::new(),
                },
            );
        return auth_redirect(config, &format!("{base}{pending}"), Some((&key, 28800)));
    }
    Err(ReviewError::new(
        StatusCode::NOT_FOUND,
        "Risorsa non disponibile",
    ))
}

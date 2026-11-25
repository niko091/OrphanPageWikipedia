use super::{auth::session_cookie, http::same_origin_json, *};
use crate::store::PageQuery;
use ring::hmac;

#[derive(Serialize, Deserialize)]
pub(super) struct VoteContext {
    pub(super) dataset: String,
    pub(super) language: String,
    pub(super) target_id: u64,
    pub(super) sort_order: String,
    pub(super) list_page: u64,
    pub(super) page_position: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    context: String,
    signature: [u8; 32],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VoteSubmission {
    stars: u8,
    receipt: Receipt,
}

pub(super) struct VoteSession {
    user_id: u64,
    cookie: String,
    key: hmac::Key,
}

impl VoteSession {
    fn signed_message(&self, context: &str) -> Vec<u8> {
        let mut message = Vec::with_capacity(self.cookie.len() + context.len());
        message.extend_from_slice(self.cookie.as_bytes());
        message.extend_from_slice(context.as_bytes());
        message
    }
}

pub(super) fn session(app: &ReviewApplication, request: &Request) -> ReviewResult<VoteSession> {
    let unauthorized =
        || ReviewError::new(StatusCode::UNAUTHORIZED, "Accedi con Wikimedia per votare.");
    let oauth = app.oauth.as_ref().ok_or_else(unauthorized)?;
    let cookie = session_cookie(request).ok_or_else(unauthorized)?;
    same_origin_json(app, request)?;
    let user_id = app
        .session(request)?
        .and_then(|identity| identity.user_id)
        .ok_or_else(unauthorized)?;
    Ok(VoteSession {
        user_id,
        cookie: cookie.to_owned(),
        key: oauth.vote_key.clone(),
    })
}

pub(super) fn attach(
    database: &ReviewDatabase,
    session: &VoteSession,
    query: &PageQuery,
    result: &mut Value,
) {
    let metadata = &result["metadata"];
    if query.kind != "cards" || metadata["complete"] != true || metadata["demo"] == true {
        return;
    }
    // The card view uses individual editions. Keys in the vote map are local page IDs.
    let Some(language) = metadata["language"].as_str().filter(|s| *s != "all") else {
        return;
    };
    result["orphan_voting"] =
        receipts(database, session, query, language, &result["pages"]).unwrap_or(Value::Null);
}

fn receipts(
    database: &ReviewDatabase,
    session: &VoteSession,
    query: &PageQuery,
    language: &str,
    pages: &Value,
) -> ReviewResult<Value> {
    let ids = pages
        .as_array()
        .ok_or_else(ReviewError::unavailable)?
        .iter()
        .map(|page| {
            page["page_id"]
                .as_u64()
                .filter(|&id| valid_id(id))
                .ok_or_else(ReviewError::unavailable)
        })
        .collect::<ReviewResult<Vec<_>>>()?;
    if ids.is_empty() {
        return Ok(json!({}));
    }
    let votes = database
        .connect_votes()?
        .votes(session.user_id, language, &ids)?;
    let mut cards = serde_json::Map::new();
    for (index, target_id) in ids.into_iter().enumerate() {
        let context = serde_json::to_string(&VoteContext {
            dataset: query.dataset.clone(),
            language: language.to_owned(),
            target_id,
            sort_order: query.order.clone(),
            list_page: (query.offset / query.page_size()) as u64 + 1,
            page_position: index as u64 + 1,
        })
        .map_err(|_| ReviewError::unavailable())?;
        let signature = hmac::sign(&session.key, &session.signed_message(&context));
        let receipt = Receipt {
            context,
            signature: signature
                .as_ref()
                .try_into()
                .map_err(|_| ReviewError::unavailable())?,
        };
        cards.insert(
            target_id.to_string(),
            json!({"stars":votes.get(&target_id),"receipt":receipt}),
        );
    }
    Ok(Value::Object(cards))
}

pub(super) async fn handle(
    State(app): State<Arc<ReviewApplication>>,
    request: Request,
) -> Response {
    match submit(app, request).await {
        Ok(stars) => response(StatusCode::OK, json!({"stars": stars})),
        Err(error) => error.into_response(),
    }
}

async fn submit(app: Arc<ReviewApplication>, request: Request) -> ReviewResult<u8> {
    if request.method() != Method::POST {
        return Err(ReviewError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "Metodo non consentito",
        ));
    }
    let session = session(&app, &request)?;
    let body = to_bytes(request.into_body(), 2048)
        .await
        .map_err(|_| ReviewError::invalid())?;
    let body: VoteSubmission = serde_json::from_slice(&body).map_err(|_| ReviewError::invalid())?;
    if !(1..=5).contains(&body.stars) {
        return Err(ReviewError::invalid());
    }
    let Receipt { context, signature } = body.receipt;
    hmac::verify(&session.key, &session.signed_message(&context), &signature)
        .map_err(|_| ReviewError::invalid())?;
    let context: VoteContext =
        serde_json::from_str(&context).map_err(|_| ReviewError::invalid())?;
    tokio::task::spawn_blocking(move || {
        app.database
            .connect_votes()?
            .save_vote(session.user_id, &context, body.stars)?;
        Ok(body.stars)
    })
    .await
    .map_err(|_| ReviewError::unavailable())?
}

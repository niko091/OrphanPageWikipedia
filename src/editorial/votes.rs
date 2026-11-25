use super::{auth::session_cookie, http::same_origin_json, *};
use crate::store::PageQuery;
use ring::hmac;

const GUEST_SCOPE: &str = "guest:";

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
    #[serde(deserialize_with = "Option::deserialize")]
    stars: Option<u8>,
    receipt: Receipt,
}

struct VoteSession {
    user_id: u64,
    cookie: String,
}

pub(super) struct VoteAccess {
    key: hmac::Key,
    session: Option<VoteSession>,
}

fn signed_message(scope: &str, context: &str) -> Vec<u8> {
    let mut message = Vec::with_capacity(scope.len() + context.len());
    message.extend_from_slice(scope.as_bytes());
    message.extend_from_slice(context.as_bytes());
    message
}

fn unauthorized() -> ReviewError {
    ReviewError::new(StatusCode::UNAUTHORIZED, "Accedi con Wikimedia per votare.")
}

pub(super) fn access(app: &ReviewApplication, request: &Request) -> ReviewResult<VoteAccess> {
    let oauth = app.oauth.as_ref().ok_or_else(unauthorized)?;
    same_origin_json(app, request)?;
    let session = app
        .session(request)?
        .map(|identity| -> ReviewResult<VoteSession> {
            Ok(VoteSession {
                user_id: identity.user_id.ok_or_else(unauthorized)?,
                cookie: session_cookie(request).ok_or_else(unauthorized)?.to_owned(),
            })
        })
        .transpose()?;
    Ok(VoteAccess {
        key: oauth.vote_key.clone(),
        session,
    })
}

pub(super) fn attach(
    database: &ReviewDatabase,
    access: &VoteAccess,
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
        receipts(database, access, query, language, &result["pages"]).unwrap_or(Value::Null);
}

fn receipts(
    database: &ReviewDatabase,
    access: &VoteAccess,
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
    let (scope, votes) = match &access.session {
        Some(session) => (
            session.cookie.as_str(),
            database
                .connect_votes()?
                .votes(session.user_id, language, &ids)?,
        ),
        // An anonymous view can attest a rank without looking up or writing any votes.
        None => (GUEST_SCOPE, BTreeMap::new()),
    };
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
        let signature = hmac::sign(&access.key, &signed_message(scope, &context));
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

async fn submit(app: Arc<ReviewApplication>, request: Request) -> ReviewResult<Option<u8>> {
    if request.method() != Method::POST {
        return Err(ReviewError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "Metodo non consentito",
        ));
    }
    let access = access(&app, &request)?;
    let session = access.session.ok_or_else(unauthorized)?;
    let body = to_bytes(request.into_body(), 2048)
        .await
        .map_err(|_| ReviewError::invalid())?;
    let body: VoteSubmission = serde_json::from_slice(&body).map_err(|_| ReviewError::invalid())?;
    if body.stars.is_some_and(|stars| !(1..=5).contains(&stars)) {
        return Err(ReviewError::invalid());
    }
    let Receipt { context, signature } = body.receipt;
    let verified = |scope: &str| {
        hmac::verify(&access.key, &signed_message(scope, &context), &signature).is_ok()
    };
    if !(verified(&session.cookie) || body.stars.is_some() && verified(GUEST_SCOPE)) {
        return Err(ReviewError::invalid());
    }
    let context: VoteContext =
        serde_json::from_str(&context).map_err(|_| ReviewError::invalid())?;
    tokio::task::spawn_blocking(move || {
        let mut storage = app.database.connect_votes()?;
        if let Some(stars) = body.stars {
            storage.save_vote(session.user_id, &context, stars)?;
        } else {
            storage.delete_vote(session.user_id, &context)?;
        }
        Ok(body.stars)
    })
    .await
    .map_err(|_| ReviewError::unavailable())?
}

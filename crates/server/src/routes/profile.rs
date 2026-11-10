//! Profile routes — ports of:
//! - `src/app/api/profile/password/route.ts` → `POST /api/profile/password`
//! - `src/app/api/profile/link/route.ts`     → `GET|POST /api/profile/link`
//!
//! Any signed-in user may hit these — no permission key, just the
//! `AuthUser` session. The password change mirrors the TS envelope rule:
//! with `RSA_*` configured the `{current, next, ts}` payload must arrive
//! inside the hybrid envelope; plaintext is rejected. A successful change
//! signs out every *other* session (`delete_all_except`) — the session
//! that rotated the password survives.
//!
//! `link` mints one-time `/link CODE`s the bot consumes on `/start` to
//! attach a Telegram account (`purpose: 'link'` rows in `link_codes`).

use std::sync::{Mutex, OnceLock};

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use openshares_core::audit::audit;
use openshares_core::crypto::envelope::{
    decrypt_payload_async, payload_fresh, rsa_enabled, rsa_private_key_pem,
};
use openshares_core::crypto::password::{hash_password_async, verify_password_async};
use openshares_core::crypto::security::{Limiter, rate_limited};
use openshares_core::crypto::signing::parse_json_body;
use openshares_core::db::now;
use openshares_core::db::repos::{link_codes, sessions, telegram_users, users};
use openshares_core::settings;
use openshares_core::util::new_token;

use crate::body::raw_body;
use crate::error::{ApiError, Result};
use crate::extract::AuthUser;
use crate::state::AppState;

/// Route table — merged by `routes::api_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/profile/password", post(change_password))
        .route("/profile/link", get(link_state).post(link_create))
}

/* ------------------------- POST /profile/password ------------------------- */

/// `parseJsonBody<{ current?, next?, payload? }>`.
#[derive(Debug, Default, Deserialize)]
struct PasswordBody {
    current: Option<String>,
    next: Option<String>,
    payload: Option<String>,
}

/// The decrypted envelope's inner payload — `{current, next, ts}`.
#[derive(Debug, Default, Deserialize)]
struct PasswordInner {
    current: Option<String>,
    next: Option<String>,
    ts: Option<f64>,
}

async fn change_password(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    req: Request,
) -> Result<Response> {
    // `rateLimited("API_LIMITER", "pwd:<uid>")` — per-user, not per-IP.
    if rate_limited(&state.db, Limiter::Api, &format!("pwd:{}", user.id)).await {
        return Ok(too_many_requests());
    }
    // `verifySignedBody` — HMAC-checked against this session's key.
    let raw = raw_body(req, Some(&user.sid)).await?;
    let outer = parse_json_body::<PasswordBody>(&raw);
    let (current, next) = password_fields(outer).await?;

    // `next.length < 8` — UTF-16 code units, like JS `.length`.
    if next.encode_utf16().count() < 8 {
        return Err(ApiError::bad_request(
            "New password must be at least 8 characters.",
        ));
    }
    let stored = users::get_by_id(&state.db, user.id)
        .await?
        .and_then(|u| u.password_hash);
    if !verify_password_async(current, stored).await? {
        return Err(ApiError::bad_request("Current password is wrong."));
    }

    let hash = hash_password_async(next).await?;
    users::update_password_hash(&state.db, user.id, &hash).await?;
    // Sign out other devices — the session that changed the password
    // survives (TS deletes all but the peeked claims' sid).
    sessions::delete_all_except(&state.db, user.id, &user.sid).await?;

    audit(
        &state.cache,
        Some(user.id),
        "user.password.change",
        Some("user"),
        Some(&user.id.to_string()),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true })).into_response())
}

/// `(current, next)` — inside the hybrid envelope when `rsaEnabled()`,
/// read off the body otherwise (password route.ts).
async fn password_fields(body: PasswordBody) -> Result<(String, String)> {
    if !rsa_enabled() {
        return Ok((
            body.current.unwrap_or_default(),
            body.next.unwrap_or_default(),
        ));
    }
    let Some(payload) = body.payload else {
        return Err(ApiError::bad_request("Encrypted credentials required."));
    };
    let pem = rsa_private_key_pem().unwrap_or_default();
    let plain = decrypt_payload_async(payload, pem).await?;
    let inner: PasswordInner = plain.as_deref().map(parse_json_body).unwrap_or_default();
    if plain.is_none() || !inner.ts.is_some_and(payload_fresh) {
        return Err(ApiError::bad_request(
            "Request payload expired or undecryptable — retry.",
        ));
    }
    Ok((
        inner.current.unwrap_or_default(),
        inner.next.unwrap_or_default(),
    ))
}

/* ------------------------- GET /profile/link ------------------------- */

/// `{telegram_id, bot_profile}` — the current Telegram connection state:
/// the linked account's `@username`, else its `first_name`, else `null`.
async fn link_state(State(state): State<AppState>, AuthUser(user): AuthUser) -> Result<Response> {
    let tg = match &user.telegram_id {
        Some(id) => telegram_users::get_by_id(&state.db, id).await?,
        None => None,
    };
    let bot_profile = tg.and_then(|t| {
        if let Some(username) = t.username.filter(|u| !u.is_empty()) {
            Some(format!("@{username}"))
        } else {
            t.first_name
        }
    });
    Ok(Json(json!({
        "telegram_id": user.telegram_id,
        "bot_profile": bot_profile,
    }))
    .into_response())
}

/* ------------------------- POST /profile/link ------------------------- */

/// Mint a one-time `/link CODE` — 409 when a Telegram account is already
/// linked, 503 when the bot identity can't be resolved (mirrors
/// `getBotUsername().catch(() => null)` → the offline error).
async fn link_create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    req: Request,
) -> Result<Response> {
    if rate_limited(&state.db, Limiter::Api, &format!("link:{}", user.id)).await {
        return Ok(too_many_requests());
    }
    // `verifySignedBody` — the body itself is unused, but the signature
    // check still applies to this mutating route.
    let _raw = raw_body(req, Some(&user.sid)).await?;

    if user.telegram_id.is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "A Telegram account is already linked to this account.",
        ));
    }
    let Some(username) = bot_username(&state).await else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Bot is offline — check BOT_TOKEN, then try again.",
        ));
    };

    let code = new_link_code();
    link_codes::create(&state.db, &code, "link", user.id, None).await?;
    Ok(Json(json!({
        "code": code,
        "bot_username": username,
        "bot_link": format!("https://t.me/{username}"),
    }))
    .into_response())
}

/// `newToken(6).replace(/[-_]/g, "0")` — 8 base64url chars with the two
/// punctuation-ish ones flattened so the code is safe to read aloud.
fn new_link_code() -> String {
    new_token(6)
        .chars()
        .map(|c| if c == '-' || c == '_' { '0' } else { c })
        .collect()
}

/* ------------------------- bot username ------------------------- */

/// `botUsernameCache` — 10-minute process-local cache in front of the
/// persisted `bot_username` setting (handler.ts `getBotUsername`).
const BOT_USERNAME_CACHE_TTL_SECS: i64 = 10 * 60;

type CacheSlot = Mutex<Option<(i64, Option<String>)>>;

static BOT_USERNAME_CACHE: OnceLock<CacheSlot> = OnceLock::new();

fn bot_username_cache() -> &'static CacheSlot {
    BOT_USERNAME_CACHE.get_or_init(|| Mutex::new(None))
}

/// `getBotUsername` — fresh cache → persisted setting → `getMe` (which
/// re-persists). `None` when the bot can't be identified at all.
async fn bot_username(state: &AppState) -> Option<String> {
    if let Some(cached) = cached_bot_username() {
        return cached;
    }
    let username = resolve_bot_username(state).await;
    if let Ok(mut guard) = bot_username_cache().lock() {
        *guard = Some((now(), username.clone()));
    }
    username
}

fn cached_bot_username() -> Option<Option<String>> {
    let guard = bot_username_cache().lock().ok()?;
    let (at, username) = guard.as_ref()?;
    (now() - at < BOT_USERNAME_CACHE_TTL_SECS).then(|| username.clone())
}

/// The uncached half of `getBotUsername`: `storedBotUsername()` wins —
/// it avoids a Telegram round-trip and keeps share links rendering if
/// `getMe` is briefly unreachable — else `getMe` + `rememberBotUsername`.
async fn resolve_bot_username(state: &AppState) -> Option<String> {
    // `.catch(() => null)` — a DB hiccup falls through to getMe, not an error.
    if let Some(stored) = settings::stored_bot_username(&state.db)
        .await
        .ok()
        .flatten()
    {
        return Some(stored);
    }
    let tg = state.tg.as_ref()?;
    match tg.get_me().await {
        Ok(me) => {
            if let Some(username) = me.username {
                if let Err(e) = settings::remember_bot_username(&state.db, &username).await {
                    tracing::warn!(error = %e, "remember_bot_username failed");
                }
                Some(username)
            } else {
                None
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "[bot] getMe failed");
            None
        }
    }
}

/// `tooManyRequests()` — `{error}` 429 + `Retry-After: 60`.
fn too_many_requests() -> Response {
    let mut res = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "error": "Too many requests — slow down and retry shortly." })),
    )
        .into_response();
    res.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::auth::tests::{TEST_JWT_PRIVATE_PEM, TEST_JWT_PUBLIC_PEM};
    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use openshares_core::config::Config;
    use openshares_core::crypto::jwt::SessionJwt;
    use openshares_core::crypto::password::{
        create_session, get_session_user_by_token, hash_password,
    };
    use openshares_core::db::Db;
    use openshares_core::db::repos::users::{self, NewUser};
    use openshares_core::media::{CacheBackend, FsCache};
    use openshares_core::util::audit_shard_key;
    use std::sync::Arc;
    use tower::ServiceExt;

    /* ------------------------- pure helpers ------------------------- */

    #[test]
    fn link_code_alphabet() {
        for _ in 0..64 {
            let code = new_link_code();
            assert_eq!(code.len(), 8, "newToken(6) → 8 base64url chars");
            assert!(
                code.bytes().all(|b| b.is_ascii_alphanumeric()),
                "- and _ flattened to 0: {code}"
            );
        }
    }

    #[tokio::test]
    async fn too_many_requests_shape() {
        let res = too_many_requests();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(res.headers().get(header::RETRY_AFTER).unwrap(), "60");
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body["error"],
            "Too many requests — slow down and retry shortly."
        );
    }

    /* ------------------------- full flow ------------------------- */

    async fn test_state(dir: &tempfile::TempDir) -> AppState {
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("t.db").display()))
            .await
            .unwrap();
        db.migrate().await.unwrap();
        AppState {
            db,
            cache: CacheBackend::Fs(FsCache::new(dir.path().join("cache"))),
            config: Config {
                bot_token: None,
                app_url: "http://localhost:8080".to_string(),
                database_url: String::new(),
                turso_db_url: None,
                turso_auth_token: None,
                media_cache_dir: dir.path().join("cache").display().to_string(),
                media_cache_backend: "fs".to_string(),
                r2_account_id: None,
                r2_access_key_id: None,
                r2_secret_access_key: None,
                r2_bucket: None,
                r2_endpoint: None,
                cache_ttl_days: 7,
                port: 0,
                web_origin: None,
            },
            tg: None,
            jwt: Some(Arc::new(
                SessionJwt::from_pems(TEST_JWT_PRIVATE_PEM, TEST_JWT_PUBLIC_PEM).unwrap(),
            )),
        }
    }

    /// User + live session → the `os_session` cookie value for requests.
    async fn session_cookie(state: &AppState) -> (i64, String, String) {
        let uid = users::create(
            &state.db,
            &NewUser {
                email: Some("u@x.co"),
                name: "User",
                password_hash: Some(&hash_password("current-pw").unwrap()),
            },
        )
        .await
        .unwrap();
        let sid = create_session(&state.db, uid).await.unwrap();
        let jwt = state.jwt.as_ref().unwrap();
        let token = jwt.sign(uid, &sid, 3600).unwrap();
        (uid, sid, token)
    }

    fn authed_post(path: &str, token: &str, body: &str) -> Request {
        Request::builder()
            .method("POST")
            .uri(path)
            .header(header::COOKIE, format!("os_session={token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn res_json(res: Response) -> serde_json::Value {
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Plaintext password change end-to-end: wrong current → 400, short
    /// next → 400, happy path → 200 + hash rotated + other sessions dead.
    #[tokio::test]
    async fn change_password_flow() {
        let _guard = crate::routes::auth::tests::ENV_LOCK.lock().await;
        unsafe {
            std::env::remove_var("RSA_PRIVATE_KEY");
            std::env::remove_var("RSA_PUBLIC_KEY");
            std::env::remove_var("REQUEST_SIGNING_SECRET");
        }
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        let (uid, _sid, token) = session_cookie(&state).await;
        // a second session on "another device"
        let other_sid = create_session(&state.db, uid).await.unwrap();
        let other_token = state
            .jwt
            .as_ref()
            .unwrap()
            .sign(uid, &other_sid, 3600)
            .unwrap();
        let app = Router::new()
            .nest("/api", router())
            .with_state(state.clone());

        // wrong current → 400
        let res = app
            .clone()
            .oneshot(authed_post(
                "/api/profile/password",
                &token,
                r#"{"current":"nope","next":"newpassword1"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(res_json(res).await["error"], "Current password is wrong.");

        // short next → 400
        let res = app
            .clone()
            .oneshot(authed_post(
                "/api/profile/password",
                &token,
                r#"{"current":"current-pw","next":"short"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res_json(res).await["error"],
            "New password must be at least 8 characters."
        );

        // happy path → 200; hash rotated; other session killed, this one lives
        let res = app
            .clone()
            .oneshot(authed_post(
                "/api/profile/password",
                &token,
                r#"{"current":"current-pw","next":"newpassword1"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res_json(res).await["ok"], true);

        let row = users::get_by_id(&state.db, uid).await.unwrap().unwrap();
        assert!(openshares_core::crypto::password::verify_password(
            "newpassword1",
            row.password_hash.as_deref()
        ));
        let jwt = state.jwt.as_ref().unwrap();
        assert!(
            get_session_user_by_token(&state.db, jwt, &token)
                .await
                .unwrap()
                .is_some(),
            "the session that changed the password survives"
        );
        assert!(
            get_session_user_by_token(&state.db, jwt, &other_token)
                .await
                .unwrap()
                .is_none(),
            "other devices signed out"
        );

        // audit line landed in the monthly JSONL shard
        let shard = state
            .cache
            .read(&audit_shard_key(now()))
            .await
            .unwrap()
            .unwrap();
        let text = String::from_utf8(shard).unwrap();
        assert!(text.contains("user.password.change"));
    }

    /// `GET /api/profile/link` reports nulls for an unlinked account, and
    /// POST mints a code once `bot_username` is resolvable (persisted
    /// setting path — no `TgClient` needed). One test so the shared
    /// 10-minute bot-username cache can't race across cases.
    #[tokio::test]
    async fn link_state_and_create() {
        let _guard = crate::routes::auth::tests::ENV_LOCK.lock().await;
        unsafe {
            std::env::remove_var("REQUEST_SIGNING_SECRET");
        }
        if let Ok(mut guard) = bot_username_cache().lock() {
            *guard = None;
        }
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        let (_uid, _sid, token) = session_cookie(&state).await;
        let app = Router::new()
            .nest("/api", router())
            .with_state(state.clone());

        // GET on an unlinked account → nulls
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/profile/link")
                    .header(header::COOKIE, format!("os_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res_json(res).await;
        assert_eq!(body["telegram_id"], serde_json::Value::Null);
        assert_eq!(body["bot_profile"], serde_json::Value::Null);

        // POST with no resolvable bot identity → 503
        let res = app
            .clone()
            .oneshot(authed_post("/api/profile/link", &token, "{}"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            res_json(res).await["error"],
            "Bot is offline — check BOT_TOKEN, then try again."
        );
        if let Ok(mut guard) = bot_username_cache().lock() {
            *guard = None;
        }

        // persisted username → POST mints a code + t.me deep link
        settings::remember_bot_username(&state.db, "testbot")
            .await
            .unwrap();
        let res = app
            .oneshot(authed_post("/api/profile/link", &token, "{}"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res_json(res).await;
        let code = body["code"].as_str().unwrap();
        assert_eq!(code.len(), 8);
        assert!(code.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_eq!(body["bot_username"], "testbot");
        assert_eq!(body["bot_link"], "https://t.me/testbot");
    }
}

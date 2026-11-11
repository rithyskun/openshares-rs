//! `GET /api/me` — port of `src/app/api/me/route.ts`.
//!
//! The signed-in user's identity + effective permission keys — powers the
//! client-side permission UI (`usePermissions().can`, the v-can
//! equivalent) and re-issues the request-signing key for tabs that lost
//! sessionStorage. Unauthenticated calls get the `unauthenticated()`
//! 401 — with the `TOKEN_EXPIRED` code + `x-session-expired` header when
//! a dead cookie was presented, which is what the frontend watches for to
//! bounce to `/login`.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use openshares_core::crypto::signing::signing_key_for;

use crate::error::Result;
use crate::extract::{MaybeUser, SESSION_COOKIE, cookie_value};
use crate::openapi::{ErrorBody, MeResponse};
use crate::state::AppState;

/// Route table — merged by `routes::api_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new().route("/me", get(me))
}

/// `Response.json({id, name, email, telegram_id, permissions, signingKey})`
/// — `signingKey` is `null` when `REQUEST_SIGNING_SECRET` is unset.
#[utoipa::path(
    get,
    path = "/api/me",
    tag = "account",
    responses(
        (status = 200, description = "Signed-in identity + sorted permission keys", body = MeResponse),
        (status = 401, description = "No session (`UNAUTHENTICATED`) or dead cookie (`TOKEN_EXPIRED` + `x-session-expired` header)", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn me(
    State(_state): State<AppState>,
    MaybeUser(user): MaybeUser,
    headers: HeaderMap,
) -> Result<Response> {
    let Some(user) = user else {
        return Ok(unauthenticated(&headers));
    };

    // `[...permissions]` — sorted so the wire shape is deterministic
    // (the TS Set's order is unspecified anyway).
    let mut permissions: Vec<String> = user.permissions.iter().cloned().collect();
    permissions.sort();

    Ok(Json(json!({
        "id": user.id,
        "name": user.name,
        "email": user.email,
        "telegram_id": user.telegram_id,
        "permissions": permissions,
        "signingKey": signing_key_for(&user.sid),
        // The session row's created_at — powers the "Signed in" stamp.
        "sessionCreatedAt": user.session_created_at,
    }))
    .into_response())
}

/// `unauthenticated()` — 401 `{error, code}`. A presented-but-dead
/// session cookie flips `code` to `TOKEN_EXPIRED` and adds the
/// `x-session-expired` marker; never-signed-in stays `UNAUTHENTICATED`.
fn unauthenticated(headers: &HeaderMap) -> Response {
    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // `!!cookie.value` — a present-but-empty value doesn't count.
    let had_cookie = cookie_value(cookie_header, SESSION_COOKIE).is_some_and(|v| !v.is_empty());
    let code = if had_cookie {
        "TOKEN_EXPIRED"
    } else {
        "UNAUTHENTICATED"
    };
    let mut res = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "Not signed in.", "code": code })),
    )
        .into_response();
    if had_cookie {
        res.headers_mut()
            .insert("x-session-expired", HeaderValue::from_static("true"));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use openshares_core::config::Config;
    use openshares_core::db::Db;
    use openshares_core::media::{CacheBackend, FsCache};
    use tower::ServiceExt;

    #[tokio::test]
    async fn unauthenticated_shape_without_cookie() {
        let res = unauthenticated(&HeaderMap::new());
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(res.headers().get("x-session-expired").is_none());
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body,
            json!({ "error": "Not signed in.", "code": "UNAUTHENTICATED" })
        );
    }

    #[tokio::test]
    async fn unauthenticated_shape_with_dead_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("os_session=dead.jwt.here; theme=dark"),
        );
        let res = unauthenticated(&headers);
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(res.headers().get("x-session-expired").unwrap(), "true");
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body,
            json!({ "error": "Not signed in.", "code": "TOKEN_EXPIRED" })
        );

        // present-but-empty value is falsy in TS — stays UNAUTHENTICATED
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("os_session="));
        let res = unauthenticated(&headers);
        assert!(res.headers().get("x-session-expired").is_none());
    }

    /// `GET /api/me` end-to-end on a real JWT state: no cookie → 401
    /// `UNAUTHENTICATED`, dead cookie → 401 `TOKEN_EXPIRED` +
    /// `x-session-expired`, live session → the identity payload.
    #[tokio::test]
    async fn me_end_to_end() {
        use openshares_core::crypto::jwt::SessionJwt;
        use openshares_core::crypto::password::{create_session, hash_password};
        use openshares_core::db::repos::users::{self, NewUser};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("t.db").display()))
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let jwt = Arc::new(
            SessionJwt::from_pems(
                super::super::auth::tests::TEST_JWT_PRIVATE_PEM,
                super::super::auth::tests::TEST_JWT_PUBLIC_PEM,
            )
            .unwrap(),
        );
        let state = AppState {
            db,
            cache: CacheBackend::Fs(FsCache::new(dir.path().join("cache"))),
            config: Config {
                bot_token: None,
                app_url: "http://localhost:8080".to_string(),
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
            jwt: Some(jwt.clone()),
        };
        // signingKey is null when REQUEST_SIGNING_SECRET is unset —
        // force that deterministically for the assertion below.
        let _guard = super::super::auth::tests::ENV_LOCK.lock().await;
        unsafe {
            std::env::remove_var("REQUEST_SIGNING_SECRET");
        }
        let uid = users::create(
            &state.db,
            &NewUser {
                email: Some("u@x.co"),
                name: "User",
                password_hash: Some(&hash_password("pw").unwrap()),
            },
        )
        .await
        .unwrap();
        let app = Router::new()
            .nest("/api", router())
            .with_state(state.clone());

        // no cookie → UNAUTHENTICATED
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/me")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(res.headers().get("x-session-expired").is_none());
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "UNAUTHENTICATED");

        // dead cookie → TOKEN_EXPIRED + marker header
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/me")
                    .header(header::COOKIE, "os_session=dead.jwt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(res.headers().get("x-session-expired").unwrap(), "true");
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "TOKEN_EXPIRED");

        // live session → identity payload (permissions empty — no roles)
        let sid = create_session(&state.db, uid).await.unwrap();
        let token = jwt.sign(uid, &sid, 3600).unwrap();
        let res = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/me")
                    .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["id"], uid);
        assert_eq!(body["name"], "User");
        assert_eq!(body["email"], "u@x.co");
        assert_eq!(body["telegram_id"], serde_json::Value::Null);
        assert_eq!(body["permissions"], json!([]));
        // REQUEST_SIGNING_SECRET unset → null signingKey
        assert_eq!(body["signingKey"], serde_json::Value::Null);
    }
}

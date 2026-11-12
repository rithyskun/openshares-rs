//! Session auth routes — ports of the `/api/auth/*` handlers:
//! - `src/app/api/auth/login/route.ts`   → `POST /api/auth/login`
//! - `src/app/api/auth/logout/route.ts`  → `POST /api/auth/logout`
//! - `src/app/api/auth/rsa-key/route.ts` → `GET  /api/auth/rsa-key`
//!
//! Login accepts `{email, password}` plaintext only while RSA is
//! unconfigured; once `RSA_PRIVATE_KEY`/`RSA_PUBLIC_KEY` are set the
//! credentials must arrive inside the hybrid envelope (`{payload}`) —
//! mandatory, not opportunistic, exactly like the TS route. The session
//! is a `sessions` row (revocation anchor) plus an RS256 `os_session`
//! httpOnly cookie carrying `{sub, sid}`.
//!
//! These paths are sign-exempt (`/api/auth/*` in `SIGN_EXEMPT`) so
//! [`json_body`] reduces to the tolerant `parseJsonBody` — and CSRF +
//! the `MW_AUTH` middleware tier still apply, same as middleware.ts.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use openshares_core::crypto::envelope::{
    decrypt_payload_async, payload_fresh, rsa_enabled, rsa_key_id, rsa_private_key_pem,
    rsa_public_key_pem,
};
use openshares_core::crypto::password::{
    SESSION_TTL_SECS, cleanup_expired_sessions, create_session, destroy_session_by_token,
    verify_password_async,
};
use openshares_core::crypto::security::{Limiter, rate_limited};
use openshares_core::crypto::signing::{parse_json_body, signing_key_for};
use openshares_core::db::repos::users;

use crate::body::json_body;
use crate::error::{ApiError, Result};
use crate::extract::{SESSION_COOKIE, cookie_value};
use crate::openapi::{ErrorBody, LoginResponse, OkBody, RsaKeyResponse};
use crate::state::AppState;

/// Route table — merged by `routes::api_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/rsa-key", get(rsa_key))
}

/* ------------------------- POST /auth/login ------------------------- */

/// `parseJsonBody<{ email?, password?, payload? }>` — unknown fields
/// ignored, garbage tolerated (a non-object body yields the defaults).
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct LoginBody {
    /// Plaintext — only while `rsaEnabled()` is false.
    email: Option<String>,
    password: Option<String>,
    /// Hybrid-envelope ciphertext carrying `{email, password, ts}` —
    /// mandatory once `RSA_*` is configured.
    payload: Option<String>,
}

/// The decrypted envelope's inner payload — `{email, password, ts}`.
#[derive(Debug, Default, Deserialize)]
struct LoginInner {
    email: Option<String>,
    password: Option<String>,
    /// `Date.now()` at encrypt time — freshness-checked ±5 min.
    ts: Option<f64>,
}

/// `POST /api/auth/login` — plaintext `{email, password}` while RSA is
/// unconfigured; `{payload}` (envelope-encrypted credentials, ±5 min
/// freshness) once it is. CSRF-checked, sign-exempt.
#[utoipa::path(
    post,
    path = "/api/auth/login",
    tag = "auth",
    request_body = LoginBody,
    responses(
        (status = 200, description = "Signed in", body = LoginResponse, headers(
            ("set-cookie" = String, description = "`os_session=<jwt>`; HttpOnly; SameSite=Lax; Max-Age=86400"),
        )),
        (status = 400, description = "Missing credentials / undecryptable or stale payload", body = ErrorBody),
        (status = 401, description = "Wrong email or password", body = ErrorBody),
        (status = 429, description = "Login rate limit — `Retry-After: 60`", body = ErrorBody),
        (status = 503, description = "Authentication is not configured", body = ErrorBody),
    ),
    security(())
)]
pub(crate) async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    req: Request,
) -> Result<Response> {
    // Route-level `AUTH_LIMITER` — the middleware's `MW_AUTH` IP tier
    // already ran; this is the route.ts `rateLimited("login:<ip>")`.
    if rate_limited(&state.db, Limiter::Auth, &login_key(&req)).await {
        return Ok(too_many_requests());
    }

    let body = json_body::<LoginBody>(req, None).await?;
    let (email, password) = credentials(body).await?;
    if email.is_empty() || password.is_empty() {
        return Err(ApiError::bad_request("Email and password are required."));
    }

    // findUserByEmail + verifyPassword — same 401 for unknown user and
    // bad password. Unknown users still pay a scrypt verify against
    // DEAD_HASH so response time doesn't leak account existence.
    let Some(user) = users::get_by_email(&state.db, &email).await? else {
        let _ = verify_password_async(password, Some(DEAD_HASH.to_string())).await;
        return Err(ApiError::unauthorized("Wrong email or password."));
    };
    if !verify_password_async(password, user.password_hash).await? {
        return Err(ApiError::unauthorized("Wrong email or password."));
    }

    let Some(jwt) = state.jwt.as_ref() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication is not configured.",
        ));
    };
    // createSession — the DB row first, then the JWT that names it.
    let sid = create_session(&state.db, user.id).await?;
    let token = jwt.sign(user.id, &sid, SESSION_TTL_SECS)?;
    // `cleanupExpiredSessions().catch(() => {})` — best-effort sweep.
    if let Err(e) = cleanup_expired_sessions(&state.db).await {
        tracing::warn!(error = %e, "cleanup_expired_sessions failed");
    }

    let cookie = session_cookie(&token, request_is_https(&headers, &uri));
    Ok(login_response(signing_key_for(&sid), &cookie))
}

/// `DEAD_HASH` — a real `salt:hash` scrypt row for a password that never
/// matches, so unknown-email logins run the same ~50 ms verify as real
/// ones. Without it the missing scrypt call leaks which emails exist
/// (the TS app had the same oracle; fixed here instead of ported).
const DEAD_HASH: &str = "a1b2c3d4e5f6071829304a5b6c7d8e9f:2fec8453c5d76c0b78d1ecd206d65f8d13ccaaaa49466aef6ad5c3b13d58d29e068220d811081b751ca71e2a9b3b822428ff7fd3068191b722885465006011a5";

/// `{email, password}` — inside the hybrid envelope when `rsaEnabled()`,
/// read off the body otherwise. The `ts` freshness check rejects stale
/// or undecryptable payloads with the TS 400.
async fn credentials(body: LoginBody) -> Result<(String, String)> {
    if !rsa_enabled() {
        return Ok((
            body.email.map(|e| e.trim().to_string()).unwrap_or_default(),
            body.password.unwrap_or_default(),
        ));
    }
    let Some(payload) = body.payload else {
        // Credentials must arrive inside the envelope — never plaintext.
        return Err(ApiError::bad_request("Encrypted credentials required."));
    };
    let pem = rsa_private_key_pem().unwrap_or_default();
    let plain = decrypt_payload_async(payload, pem).await?;
    let inner: LoginInner = plain.as_deref().map(parse_json_body).unwrap_or_default();
    if plain.is_none() || !inner.ts.is_some_and(payload_fresh) {
        return Err(ApiError::bad_request(
            "Login payload expired or undecryptable — retry.",
        ));
    }
    Ok((
        inner
            .email
            .map(|e| e.trim().to_string())
            .unwrap_or_default(),
        inner.password.unwrap_or_default(),
    ))
}

/// `"login:" + clientIp(req)` — the route-level limiter bucket key.
/// Reads the [`ClientIp`] the gateway resolved (trusted-proxy aware),
/// never the raw forwarding headers.
fn login_key(req: &Request) -> String {
    let ip = req
        .extensions()
        .get::<crate::extract::ClientIp>()
        .map(|c| c.0.as_str())
        .unwrap_or("unknown");
    format!("login:{ip}")
}

/* ------------------------- POST /auth/logout ------------------------- */

/// `destroySession` — peek the (possibly expired) JWT, delete the
/// session row it names, and expire the cookie unconditionally.
#[utoipa::path(
    post,
    path = "/api/auth/logout",
    tag = "auth",
    responses(
        (status = 200, description = "Session destroyed", body = OkBody, headers(
            ("set-cookie" = String, description = "`os_session=`; Max-Age=0 (expired)"),
        )),
    ),
    security(())
)]
pub(crate) async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response> {
    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if let Some(token) = cookie_value(cookie_header, SESSION_COOKIE) {
        destroy_session_by_token(&state.db, token).await?;
    }
    Ok(expire_session_response())
}

/* ------------------------- GET /auth/rsa-key ------------------------- */

/// `{enabled, publicKey, keyId}` — the key the login/password forms
/// encrypt credentials with. `{enabled: false}` when RSA isn't
/// configured so dev setups can still log in over plaintext.
#[utoipa::path(
    get,
    path = "/api/auth/rsa-key",
    tag = "auth",
    responses(
        (status = 200, description = "RSA public key for credential encryption", body = RsaKeyResponse),
    ),
    security(())
)]
pub(crate) async fn rsa_key() -> Json<serde_json::Value> {
    match (rsa_enabled(), rsa_public_key_pem()) {
        (true, Some(public_key)) => Json(json!({
            "enabled": true,
            "publicKey": public_key,
            "keyId": rsa_key_id(&public_key),
        })),
        _ => Json(json!({ "enabled": false })),
    }
}

/* ------------------------- cookies + responses ------------------------- */

/// `createSession`'s cookie write — `os_session=<jwt>`; HttpOnly;
/// `SameSite=Lax` (top-level navigations carry the session — Strict
/// would drop it when a share/external link opens the app);
/// `Max-Age=SESSION_TTL`; `Secure` over https.
fn session_cookie(token: &str, secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; Max-Age={SESSION_TTL_SECS}; SameSite=Lax{}; HttpOnly",
        if secure { "; Secure" } else { "" },
    )
}

/// `store.delete(SESSION_COOKIE)` — `Max-Age=0` clears the cookie.
fn expired_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly")
}

/// `req.nextUrl.protocol === "https:"` — behind a reverse proxy the
/// scheme arrives via `x-forwarded-proto` (same helper shape as the
/// like-cookies in `routes/public_album.rs`).
fn request_is_https(headers: &HeaderMap, uri: &Uri) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or_else(|| uri.scheme_str() == Some("https"))
}

/// `{ok: true, signingKey}` + the `os_session` `Set-Cookie`.
/// `signingKey` is `null` when `REQUEST_SIGNING_SECRET` is unset.
fn login_response(signing_key: Option<String>, cookie: &str) -> Response {
    let body = json!({ "ok": true, "signingKey": signing_key });
    match HeaderValue::from_str(cookie) {
        Ok(value) => ([(header::SET_COOKIE, value)], Json(body)).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "invalid Set-Cookie value — session cookie skipped");
            Json(body).into_response()
        }
    }
}

/// `{ok: true}` + the expired `os_session` cookie.
fn expire_session_response() -> Response {
    let body = json!({ "ok": true });
    match HeaderValue::from_str(&expired_session_cookie()) {
        Ok(value) => ([(header::SET_COOKIE, value)], Json(body)).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "invalid Set-Cookie value");
            Json(body).into_response()
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

/// `pub(crate)` so sibling route tests can reuse the JWT test pair.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use openshares_core::config::Config;
    use openshares_core::crypto::jwt::SessionJwt;
    use openshares_core::crypto::password::{get_session_user_by_token, hash_password};
    use openshares_core::db::Db;
    use openshares_core::db::repos::users::NewUser;
    use openshares_core::media::{CacheBackend, FsCache};
    use std::sync::Arc;
    use tower::ServiceExt;

    /// Serializes tests that mutate process env (`RSA_*`,
    /// `REQUEST_SIGNING_SECRET`) — `rsa_enabled()`/`signing_key_for`
    /// read env at call time, so set/remove pairs must not interleave
    /// across parallel tests. `pub(crate)` — the `me`/`profile` tests
    /// lock the same mutex (tokio's guard is `Send`, safe across await).
    pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Disposable RSA-2048 pair for `SessionJwt::from_pems` in tests —
    /// generated with openssl for this file, never deployed. `pub(crate)`
    /// so sibling route tests (`me`, `profile`) share the pair.
    pub(crate) const TEST_JWT_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC3xJLJJAndY7y3\n\
Q77BnWVaExC/uT7g92e1JfwDL47pkYq3opQn6co6CeBHWw7audeBcZNtzmqu5QBU\n\
vHMcfpfO1j7YziBBQ13LzsJ1iTaTvy8mbjLGs1fq2WlYTM4kFbBXwzcEumP9oTOp\n\
bgM9+n6oBhS+g9Z//Jw0723bSl5ffN4TiwNN3Vr7+HHMYbA61G2su3rXlPvylvFo\n\
iFAduFSxo9l0xViqs8SxyzXAlac0okfqyGB+e/DL9Wr6GN6RlSEaryN6am546F+O\n\
i5HGDivnmqURlvMAD3QEgEBOMgforJoIMShbD3T6wZtcfS+wNARt9maLnpAm46Ah\n\
xzwGhkQ3AgMBAAECggEAW69SEtONrmy393Fvx1aZfga8GQO56PDWx6zTXt2H/mV0\n\
YalWJNHCaRGLWd6G+z60VCtPxH2zgV3Uu/IntmQ8pmADDBVPR3BuaCRYpd6XIcDF\n\
q/VYFQXei9f79KehQ6SkjiziEtRR1fVq+pHeQTpsnbUm/WR2jPWCE/EPDNU+X4Hq\n\
t/fn4XkNabuxmWwp51tZL7onUl+jLMEKFRwROf+iFzPxrqnSbyYAZoKMcD5qhKIj\n\
9Ls+qy2XhpYM6SCH6YHyXtUkw7+cZc1tYQ+EByrnOGuGmBcDRN75JwVv8t5kXpqu\n\
OBss0QjJZ0PlKt3ITIFYzC73MVtMdTyXOVmYzrfKYQKBgQDoDUccrGPp7M/ojIEE\n\
ifz7UWNsjYmHpSjpwL57kkgoT8XQPCv9oS96waut9+QdJoFVRJUFEXUlMEIfPH9f\n\
gA24CHwxQ7SzSA2dCLTCbqcvwx5q07WGW6sqsexG24Ty2eD75Cr+CSl7Gu+Vrd96\n\
CLO5DPU7poDaFkR93D07fl3DlwKBgQDKu6aORQyrYLU9FLFb+uW7I0d/Ehu6KyC1\n\
ZS7Ygn2Gjty/venGIpA2aFfY58n0+uwNVifeTEM6fGnpVAp5Nzp07iwTPBz0wLta\n\
3MTmTK43mZY7iVzI3mvDZNLYIrdcPgKFvAIWOmnVn8Ai9zjMvqS/2hhV5yjakqio\n\
mKQhm78YYQKBgDxd3+cErcVYFsofaxzcLNTQWnYN5/5Cp/8PZj5NUrox7nr6fFMx\n\
0GE16syKFWzD7JdbDQ1RelEfWcQ4f8Y/UNJUWsKSI3sUPn5gmm2OTa3uK4aM5sc6\n\
J7AmsxnH17E5nv7IX6rQhVsCKdudbOqZQu/cABvxd8nESzMWzVMhz5CnAoGAN1qB\n\
IXtM+EYRdjn+KyUYML5zOQjStUQtTBnybn9CksjTES9jcwvg4FohEkJHbDJO3V1W\n\
yWV9PazWpjSiH3Kq7V5SfnX9Wf59qVN6F398WgX/dQYOZXIFbnZK82SXOAJeYfFx\n\
iGwulXtm1KU3xv+OsCt2VY4mI3fDwTTcK6kAxkECgYEAxfdpIbDT6DTKmO4n3WBz\n\
0o2kIFvXoN53Vdtmq3j21Vi5nXP9Fp7dWDJw4Eaw9qjUnzvjPTtog8wJh9vsn3Ty\n\
wSpGtTlLH+fHP3ZutGmQzc51DBMbgoqpeyTh4v+yhPGsT/gfV/11rJKHsvNlnWwC\n\
oHuks+ovlN3Rrzprjg1/gms=\n\
-----END PRIVATE KEY-----\n";

    pub(crate) const TEST_JWT_PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAt8SSySQJ3WO8t0O+wZ1l\n\
WhMQv7k+4PdntSX8Ay+O6ZGKt6KUJ+nKOgngR1sO2rnXgXGTbc5qruUAVLxzHH6X\n\
ztY+2M4gQUNdy87CdYk2k78vJm4yxrNX6tlpWEzOJBWwV8M3BLpj/aEzqW4DPfp+\n\
qAYUvoPWf/ycNO9t20peX3zeE4sDTd1a+/hxzGGwOtRtrLt615T78pbxaIhQHbhU\n\
saPZdMVYqrPEscs1wJWnNKJH6shgfnvwy/Vq+hjekZUhGq8jempueOhfjouRxg4r\n\
55qlEZbzAA90BIBATjIH6KyaCDEoWw90+sGbXH0vsDQEbfZmi56QJuOgIcc8BoZE\n\
NwIDAQAB\n\
-----END PUBLIC KEY-----\n";

    /* ------------------------- pure helpers ------------------------- */

    #[test]
    fn session_cookie_shape() {
        assert_eq!(
            session_cookie("tok", false),
            "os_session=tok; Path=/; Max-Age=86400; SameSite=Lax; HttpOnly"
        );
        assert_eq!(
            session_cookie("tok", true),
            "os_session=tok; Path=/; Max-Age=86400; SameSite=Lax; Secure; HttpOnly"
        );
        assert_eq!(
            expired_session_cookie(),
            "os_session=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly"
        );
    }

    #[test]
    fn https_detection() {
        let uri: Uri = "https://example.com/api/auth/login".parse().unwrap();
        assert!(request_is_https(&HeaderMap::new(), &uri));

        let http_uri: Uri = "/api/auth/login".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(request_is_https(&headers, &http_uri));

        headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
        assert!(!request_is_https(&headers, &uri));
        assert!(!request_is_https(&HeaderMap::new(), &http_uri));
    }

    #[test]
    fn login_key_uses_gateway_resolved_ip() {
        // The gateway resolves the client IP into `ClientIp` — raw
        // forwarding headers are never re-consulted here.
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .extension(crate::extract::ClientIp("1.2.3.4".to_string()))
            .body(Body::empty())
            .unwrap();
        assert_eq!(login_key(&req), "login:1.2.3.4");

        // A spoofed cf-connecting-ip can't influence the bucket when the
        // gateway didn't install a ClientIp (or resolved something else).
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("cf-connecting-ip", "9.9.9.9")
            .body(Body::empty())
            .unwrap();
        assert_eq!(login_key(&req), "login:unknown");
    }

    #[tokio::test]
    async fn credentials_plaintext_when_rsa_off() {
        let _guard = ENV_LOCK.lock().await;
        // Safety net for the ambient env: plaintext is only allowed while
        // `rsa_enabled()` is false, so force it for this test's duration.
        unsafe {
            std::env::remove_var("RSA_PRIVATE_KEY");
            std::env::remove_var("RSA_PUBLIC_KEY");
        }
        let (email, password) = credentials(LoginBody {
            email: Some("  u@x.co ".to_string()),
            password: Some("pw".to_string()),
            payload: None,
        })
        .await
        .unwrap();
        assert_eq!((email.as_str(), password.as_str()), ("u@x.co", "pw"));
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
                trust_proxy: false,
            },
            tg: None,
            jwt: Some(Arc::new(
                SessionJwt::from_pems(TEST_JWT_PRIVATE_PEM, TEST_JWT_PUBLIC_PEM).unwrap(),
            )),
        }
    }

    fn post(path: &str, body: &str) -> Request {
        Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn res_json(res: Response) -> serde_json::Value {
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// End-to-end login + logout on the `/api`-nested router: plaintext
    /// credentials (RSA unset), cookie issued, session row created and
    /// destroyed, cookie expired.
    #[tokio::test]
    async fn login_logout_roundtrip() {
        let _guard = ENV_LOCK.lock().await;
        // Force the plaintext path regardless of the ambient env (the
        // remove is scoped to this test; nothing else reads these vars).
        unsafe {
            std::env::remove_var("RSA_PRIVATE_KEY");
            std::env::remove_var("RSA_PUBLIC_KEY");
        }
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        let uid = users::create(
            &state.db,
            &NewUser {
                email: Some("u@x.co"),
                name: "User",
                password_hash: Some(&hash_password("secret").unwrap()),
            },
        )
        .await
        .unwrap();
        let app = Router::new()
            .nest("/api", router())
            .with_state(state.clone());

        // wrong password → 401, no cookie
        let res = app
            .clone()
            .oneshot(post(
                "/api/auth/login",
                r#"{"email":"u@x.co","password":"bad"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(res.headers().get(header::SET_COOKIE).is_none());
        assert_eq!(res_json(res).await["error"], "Wrong email or password.");

        // right password → 200 {ok,signingKey} + os_session cookie
        let res = app
            .clone()
            .oneshot(post(
                "/api/auth/login",
                r#"{"email":" u@x.co ","password":"secret"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let cookie = res
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(cookie.starts_with("os_session="));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Max-Age=86400"));
        assert!(!cookie.contains("Secure")); // plain-http request
        assert_eq!(res_json(res).await["ok"], true);

        // the issued JWT resolves to a live session
        let token = cookie_value(&cookie, SESSION_COOKIE).unwrap();
        let jwt = state.jwt.as_ref().unwrap();
        let session = get_session_user_by_token(&state.db, jwt, token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.id, uid);

        // logout kills the session row and expires the cookie
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/logout")
                    .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let expired = res
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(expired.contains("Max-Age=0"));
        assert!(
            get_session_user_by_token(&state.db, jwt, token)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn login_rejects_blank_and_missing_fields() {
        let _guard = ENV_LOCK.lock().await;
        unsafe {
            std::env::remove_var("RSA_PRIVATE_KEY");
            std::env::remove_var("RSA_PUBLIC_KEY");
        }
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        let app = Router::new().nest("/api", router()).with_state(state);

        // whitespace-only email / missing password → 400
        let res = app
            .clone()
            .oneshot(post("/api/auth/login", r#"{"email":"   ","password":"x"}"#))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let res = app
            .clone()
            .oneshot(post("/api/auth/login", r#"{"email":"u@x.co"}"#))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        // garbage body → tolerated parse → same 400
        let res = app
            .oneshot(post("/api/auth/login", "not json"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res_json(res).await["error"],
            "Email and password are required."
        );
    }

    #[tokio::test]
    async fn login_envelope_required_when_rsa_on() {
        let _guard = ENV_LOCK.lock().await;
        // Setting both RSA vars flips the route to envelope-only, same
        // as `rsaEnabled()`. Removed again at the end — single test so
        // parallel cases can't observe the mutation.
        unsafe {
            std::env::set_var("RSA_PRIVATE_KEY", TEST_JWT_PRIVATE_PEM);
            std::env::set_var("RSA_PUBLIC_KEY", TEST_JWT_PUBLIC_PEM);
        }
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        let uid = users::create(
            &state.db,
            &NewUser {
                email: Some("u@x.co"),
                name: "User",
                password_hash: Some(&hash_password("secret").unwrap()),
            },
        )
        .await
        .unwrap();
        assert!(uid > 0);
        let app = Router::new().nest("/api", router()).with_state(state);

        // rsa-key advertises the public key + 12-hex key id
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/auth/rsa-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = res_json(res).await;
        assert_eq!(body["enabled"], true);
        assert_eq!(
            body["keyId"].as_str().unwrap().len(),
            12,
            "keyId is 12 hex chars"
        );

        // plaintext now rejected
        let res = app
            .clone()
            .oneshot(post(
                "/api/auth/login",
                r#"{"email":"u@x.co","password":"secret"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res_json(res).await["error"],
            "Encrypted credentials required."
        );

        // garbage envelope → undecryptable 400
        let res = app
            .clone()
            .oneshot(post("/api/auth/login", r#"{"payload":"junk"}"#))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res_json(res).await["error"],
            "Login payload expired or undecryptable — retry."
        );

        // a real envelope logs in — encrypt like the browser does
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let inner = format!(r#"{{"email":"u@x.co","password":"secret","ts":{now_ms}}}"#);
        let wire =
            openshares_core::crypto::envelope::encrypt_payload(&inner, TEST_JWT_PUBLIC_PEM, "test")
                .unwrap();
        let res = app
            .oneshot(post(
                "/api/auth/login",
                &serde_json::json!({ "payload": wire }).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().get(header::SET_COOKIE).is_some());

        unsafe {
            std::env::remove_var("RSA_PRIVATE_KEY");
            std::env::remove_var("RSA_PUBLIC_KEY");
        }
    }

    #[tokio::test]
    async fn rsa_key_disabled_without_env() {
        let _guard = ENV_LOCK.lock().await;
        unsafe {
            std::env::remove_var("RSA_PRIVATE_KEY");
            std::env::remove_var("RSA_PUBLIC_KEY");
        }
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(&dir).await;
        let app = Router::new().nest("/api", router()).with_state(state);
        let res = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/auth/rsa-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = res_json(res).await;
        assert_eq!(body, json!({ "enabled": false }));
    }
}

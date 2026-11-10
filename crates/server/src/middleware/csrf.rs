//! Double-submit CSRF — port of middleware.ts lines 39–46, 65–77 and
//! 128–140.
//!
//! Browser clients echo the JS-readable `XSRF-TOKEN` cookie back in the
//! `x-csrf-token` header via `apiFetch()`; foreign origins can't read the
//! cookie, so a matching header proves same-origin JS. The cookie is issued
//! on every pass-through response (`SameSite=Strict`, **not** httpOnly —
//! being readable is the point) and is never rotated when already present
//! (in-flight posts would fail against the new value).

use std::fmt::Write as _;

use axum::Json;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rand::RngExt;
use serde_json::json;

use openshares_core::crypto::security::secret_eq;

use crate::extract::cookie_value;

/// `CSRF_COOKIE` — the double-submit cookie name.
pub const CSRF_COOKIE: &str = "XSRF-TOKEN";
/// `CSRF_HEADER` — the echo header checked on mutations.
const CSRF_HEADER: HeaderName = HeaderName::from_static("x-csrf-token");
/// `CSRF_EXEMPT` — server-to-server endpoints with no browser cookie
/// context (prefix match, same as the TS `startsWith` loop).
const CSRF_EXEMPT: &[&str] = &["/api/bot/webhook"];

/// The `Set-Cookie` line to send, precomputed from the request — `req` is
/// consumed by `next.run`, so the decision ("needs a token?" + "https?")
/// is snapshotted beforehand. `None` when the request already carries a
/// token cookie (issueCsrfCookie never rotates).
pub struct CookiePlan(Option<HeaderValue>);

/// `issueCsrfCookie`'s request half — read the cookie jar and protocol.
pub fn plan(req: &Request) -> CookiePlan {
    if csrf_cookie(req).is_some() {
        return CookiePlan(None);
    }
    let secure = if is_https(req) { "; Secure" } else { "" };
    let line = format!(
        "{CSRF_COOKIE}={}; Path=/; SameSite=Strict{secure}",
        new_token()
    );
    CookiePlan(HeaderValue::from_str(&line).ok())
}

/// Append the planned `Set-Cookie` to a response (TS `headers.append`).
pub fn apply(res: &mut Response, plan: &CookiePlan) {
    if let Some(v) = plan.0.as_ref() {
        res.headers_mut().append(header::SET_COOKIE, v.clone());
    }
}

/// `MUTATING.has(method) && !CSRF_EXEMPT.some(startsWith)` — whether this
/// request must prove same-origin via the echo header.
pub fn requires_token(method: &Method, path: &str) -> bool {
    super::is_mutating(method) && !CSRF_EXEMPT.iter().any(|p| path.starts_with(p))
}

/// The token check: both sides present, non-empty, and `secret_eq`-equal.
pub fn validate(req: &Request) -> bool {
    let header = req
        .headers()
        .get(&CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let cookie = csrf_cookie(req).unwrap_or_default();
    !header.is_empty() && !cookie.is_empty() && secret_eq(header, &cookie)
}

/// The 403 body — `{"error": "CSRF token missing or invalid."}`.
pub fn rejected() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "CSRF token missing or invalid." })),
    )
        .into_response()
}

/// The request's `XSRF-TOKEN` cookie value, if any.
fn csrf_cookie(req: &Request) -> Option<String> {
    let jar = req.headers().get(header::COOKIE)?.to_str().ok()?;
    cookie_value(jar, CSRF_COOKIE).map(str::to_owned)
}

/// `req.nextUrl.protocol === "https:"` — direct scheme, or the
/// `x-forwarded-proto` hint a TLS-terminating proxy sets (the Rust server
/// usually sits behind one, same as the Workers deployment).
fn is_https(req: &Request) -> bool {
    req.uri().scheme_str() == Some("https")
        || req
            .headers()
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            == Some("https")
}

/// 32 random bytes → 64-char hex token (`crypto.getRandomValues(32)` + hex).
fn new_token() -> String {
    let bytes: [u8; 32] = rand::rng().random();
    let mut s = String::with_capacity(64);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn req(cookie: Option<&str>, csrf: Option<&str>) -> Request {
        let mut b = Request::builder().method("POST").uri("/api/x");
        if let Some(c) = cookie {
            b = b.header(header::COOKIE, c);
        }
        if let Some(t) = csrf {
            b = b.header(&CSRF_HEADER, t);
        }
        b.body(Body::empty()).unwrap()
    }

    #[test]
    fn requires_token_matrix() {
        // mutating + non-exempt → required
        assert!(requires_token(&Method::POST, "/api/albums"));
        assert!(requires_token(&Method::DELETE, "/api/media/1"));
        assert!(requires_token(&Method::POST, "/api/auth/login"));
        // reads never required
        assert!(!requires_token(&Method::GET, "/api/albums"));
        assert!(!requires_token(&Method::OPTIONS, "/api/albums"));
        // exempt prefix (startsWith, not exact — anything under it is exempt)
        assert!(!requires_token(&Method::POST, "/api/bot/webhook"));
        assert!(!requires_token(&Method::POST, "/api/bot/webhook/v2"));
        assert!(!requires_token(&Method::POST, "/api/bot/webhooks"));
        // ...but unrelated prefixes still require the token
        assert!(requires_token(&Method::POST, "/api/bot/other"));
    }

    #[test]
    fn validate_pairs() {
        assert!(validate(&req(Some("XSRF-TOKEN=abc"), Some("abc"))));
        // header must match the cookie, not just exist
        assert!(!validate(&req(Some("XSRF-TOKEN=abc"), Some("abd"))));
        assert!(!validate(&req(Some("XSRF-TOKEN=abc"), None)));
        assert!(!validate(&req(None, Some("abc"))));
        assert!(!validate(&req(None, None)));
        // empty header echo doesn't count
        assert!(!validate(&req(Some("XSRF-TOKEN=abc"), Some(""))));
        // other cookies around it are fine
        assert!(validate(&req(
            Some("os_session=jwt; XSRF-TOKEN=zzz; theme=dark"),
            Some("zzz")
        )));
    }

    #[test]
    fn plan_issues_only_when_absent() {
        // no cookie → plan carries a Set-Cookie with a fresh token
        let p = plan(&req(None, None));
        let v = p.0.as_ref().unwrap().to_str().unwrap().to_string();
        assert!(v.starts_with("XSRF-TOKEN="));
        assert!(v.contains("Path=/"));
        assert!(v.contains("SameSite=Strict"));
        assert!(!v.contains("HttpOnly"));
        assert!(!v.contains("Secure"));
        let token = v["XSRF-TOKEN=".len()..]
            .split(';')
            .next()
            .unwrap()
            .to_string();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));

        // cookie present → never rotated
        let p2 = plan(&req(Some("XSRF-TOKEN=keep-me"), None));
        assert!(p2.0.is_none());
    }

    #[test]
    fn plan_adds_secure_on_https() {
        let r = Request::builder()
            .method("GET")
            .uri("https://example.com/api/x")
            .body(Body::empty())
            .unwrap();
        let v = plan(&r).0.as_ref().unwrap().to_str().unwrap().to_string();
        assert!(v.contains("; Secure"));

        // forwarded-proto behind a TLS-terminating proxy counts too
        let r2 = Request::builder()
            .method("GET")
            .uri("/api/x")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .unwrap();
        let v2 = plan(&r2).0.as_ref().unwrap().to_str().unwrap().to_string();
        assert!(v2.contains("; Secure"));
    }

    #[test]
    fn apply_appends_set_cookie() {
        let p = plan(&req(None, None));
        let mut res = StatusCode::OK.into_response();
        apply(&mut res, &p);
        assert!(res.headers().get(header::SET_COOKIE).is_some());

        // empty plan appends nothing
        let none = CookiePlan(None);
        let mut res2 = StatusCode::OK.into_response();
        apply(&mut res2, &none);
        assert!(res2.headers().get(header::SET_COOKIE).is_none());
    }

    #[test]
    fn rejected_is_json_403() {
        assert_eq!(rejected().status(), StatusCode::FORBIDDEN);
    }
}

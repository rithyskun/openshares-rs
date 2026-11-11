//! Basic-auth gate for the opt-in `/docs` explorer — a Rust-only
//! addition (the TS app shipped no docs route).
//!
//! `SWAGGER_AUTH=user:password` supplies the credential pair; the full
//! expected `Authorization` header value is precomputed once at boot so
//! a request costs one constant-time compare (`subtle::ConstantTimeEq` —
//! the `secretEq` counterpart). The crate's own `basic_auth` isn't used
//! because it compares with plain string equality. Docs fail CLOSED: a
//! missing or malformed var means the router isn't mounted (main.rs).

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use subtle::ConstantTimeEq;

use crate::error::ApiError;

/// Read `SWAGGER_AUTH` and produce the expected `Authorization` value.
pub fn expected_from_env() -> Option<Arc<str>> {
    expected_header(&std::env::var("SWAGGER_AUTH").ok()?)
}

/// `user:password` → `Basic <b64>` — password may itself contain ':'
/// (split at the first). Either side empty → `None` (docs stay off).
fn expected_header(raw: &str) -> Option<Arc<str>> {
    let (user, pass) = raw.split_once(':')?;
    if user.is_empty() || pass.is_empty() {
        return None;
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
    Some(Arc::from(format!("Basic {encoded}")))
}

/// Gate: one constant-time compare of the wire header against the
/// precomputed value. Rejects with `401` + `WWW-Authenticate` (browser
/// prompt) and the app's `{ "error": … }` envelope.
pub async fn require(State(expected): State<Arc<str>>, req: Request, next: Next) -> Response {
    let ok: bool = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|given| given.as_bytes().ct_eq(expected.as_bytes()).into());
    if ok {
        return next.run(req).await;
    }
    let mut res =
        ApiError::new(StatusCode::UNAUTHORIZED, "Authentication required.").into_response();
    res.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"openshares docs\""),
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use base64::engine::general_purpose::STANDARD;
    use tower::ServiceExt;

    async fn ok() -> &'static str {
        "ok"
    }

    fn app(expected: Arc<str>) -> Router {
        Router::new()
            .route("/docs", get(ok))
            .layer(from_fn_with_state(expected, require))
    }

    fn get_req(auth: Option<&str>) -> Request {
        let mut b = Request::builder().method("GET").uri("/docs");
        if let Some(v) = auth {
            b = b.header(header::AUTHORIZATION, v);
        }
        b.body(Body::empty()).unwrap()
    }

    #[test]
    fn parses_user_colon_password() {
        let h = expected_header("docs:s3cret").unwrap();
        let want = format!("Basic {}", STANDARD.encode("docs:s3cret"));
        assert_eq!(h.as_ref(), want.as_str());
    }

    #[test]
    fn password_may_contain_colons() {
        assert!(expected_header("u:p:a:s:s").is_some());
    }

    #[test]
    fn rejects_missing_parts() {
        assert!(expected_header("no-colon").is_none());
        assert!(expected_header(":pass").is_none());
        assert!(expected_header("user:").is_none());
        assert!(expected_header("").is_none());
    }

    #[tokio::test]
    async fn no_header_401s_with_challenge() {
        let app = app(expected_header("u:p").unwrap());
        let res = app.oneshot(get_req(None)).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let wa = res.headers().get(header::WWW_AUTHENTICATE).unwrap();
        assert!(wa.to_str().unwrap().starts_with("Basic realm="));
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "Authentication required.");
    }

    #[tokio::test]
    async fn wrong_password_401s() {
        let app = app(expected_header("u:right").unwrap());
        let bad = format!("Basic {}", STANDARD.encode("u:wrong"));
        let res = app.oneshot(get_req(Some(&bad))).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn right_credentials_pass() {
        let app = app(expected_header("u:p").unwrap());
        let good = format!("Basic {}", STANDARD.encode("u:p"));
        let res = app.oneshot(get_req(Some(&good))).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}

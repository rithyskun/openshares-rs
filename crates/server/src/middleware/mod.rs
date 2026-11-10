//! Gateway middleware stack — port of `src/middleware.ts`.
//!
//! The TS file is a single function that runs, in order:
//! correlation-id → security headers → same-origin gate (`/api/*` only:
//! `Origin` must match the request host, `OPTIONS` → 204) → phase-1 IP
//! rate limit (`MW_*` tiers: auth 10/min, webhook 240/min, write 120/min,
//! read 600/min — emits `X-RateLimit-*`, `Retry-After`, and
//! `X-BruteForce-Protection` on the auth tier) → double-submit CSRF
//! (JS-readable `XSRF-TOKEN` cookie, SameSite=Strict; mutating methods must
//! echo it in `x-csrf-token`; `/api/bot/webhook` exempt). HMAC
//! request-signing stays per-route (`verifySignedBody`) — not middleware.
//!
//! # Layer order
//!
//! In axum, `.layer()` calls execute outermost-LAST, so the stack below is
//! declared inside-out. Request path:
//!
//! ```text
//! trace::access_log           (outermost — one INFO line per finished
//!   → correlation::set           request: "GET /api/… 200 12 ms")
//!     → security_headers::set  (on every response incl. early returns)
//!       → gateway              (the `/api/*` pipeline: same-origin →
//!                                OPTIONS → rate-limit → CSRF → next,
//!                                then rate-limit headers + XSRF cookie)
//!         → handler
//! ```
//!
//! Early-return parity with `setSecurity(res, correlationId)`: the two
//! outer layers decorate every response — including the gateway's
//! 403/204/429 short-circuits — while rate-limit headers and the XSRF
//! cookie are emitted only on the paths middleware.ts emits them
//! (pass-through for both; the CSRF-403 for the cookie alone).

/// `correlation` is `pub` so [`correlation::CorrelationId`] (the request
/// extension carrying the resolved id) is reachable by handlers and future
/// tracing instrumentation — the rest stay crate-private like the TS file.
pub mod correlation;
mod csrf;
mod rate_limit;
mod same_origin;
mod security_headers;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::{self, Next};
use axum::response::Response;
use tower_http::trace::{self, TraceLayer};
use tracing::Level;

use openshares_core::crypto::security::rate_limit_info;

use crate::state::AppState;

/// `pathname.startsWith("/api/")` — a bare `/api` is NOT API-scoped,
/// same as the TS prefix check.
const API_PREFIX: &str = "/api/";

/// `MUTATING` in middleware.ts — state-changing methods (CSRF risk,
/// `MW_WRITE` tier).
fn is_mutating(method: &Method) -> bool {
    [Method::POST, Method::PUT, Method::PATCH, Method::DELETE].contains(method)
}

/// Wrap `router` with the full pipeline. The returned router keeps the
/// same `AppState` — layers pull `&AppState` via `State`.
///
/// The CSP's `dev` branch (TS `NODE_ENV !== "production"`) is resolved once
/// here via [`security_headers::dev_mode`] and handed to the header layer as
/// its state.
pub fn apply(router: Router<AppState>, state: AppState) -> Router<AppState> {
    let csp: Arc<str> = security_headers::csp(security_headers::dev_mode()).into();

    router
        // innermost: the /api/* gate pipeline (needs `state.db` for the
        // rate_limits upsert).
        .layer(middleware::from_fn_with_state(state, gateway))
        // middle: SECURITY_HEADERS + CSP on every response.
        .layer(middleware::from_fn_with_state(csp, security_headers::set))
        // correlation id resolves/echoes around everything.
        .layer(middleware::from_fn(correlation::set))
        // outermost: access log — one INFO line per request incl.
        // middleware short-circuits (Next dev's "GET /x 200 in Nms" twin).
        // The span must be INFO-enabled or its method+uri fields never
        // attach; on_request is silenced so the line stays single.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(trace::DefaultMakeSpan::new().level(Level::INFO))
                .on_request(|_: &Request, _: &tracing::Span| {})
                .on_response(trace::DefaultOnResponse::new().level(Level::INFO)),
        )
}

/// The `middleware(req)` body — one function, same branch order as the TS.
///
/// Only this layer may short-circuit; the outer layers' post-processing
/// still decorates whatever it returns (that's how `setSecurity` behaves).
async fn gateway(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let plan = csrf::plan(&req);

    if path.starts_with(API_PREFIX) {
        // Same-origin-only API: any Origin header must match this host —
        // before the OPTIONS short-circuit, exactly like middleware.ts.
        if let Some(rejection) = same_origin::check(&req) {
            return rejection;
        }
        if req.method() == Method::OPTIONS {
            return same_origin::options_response();
        }

        // Phase-1 rate limit — before CSRF so floods die on the cheapest
        // check first. Fails open on DB errors (inside `rate_limit_info`).
        let tier = rate_limit::mw_tier(&path, req.method());
        let key = rate_limit::ip_key(&req);
        // Session resolution is independent of the limiter — each is its
        // own remote round trip, so overlap them (a 429 wastes one read).
        let cookie_header = req
            .headers()
            .get(axum::http::header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let (info, session) = tokio::join!(
            rate_limit_info(&state.db, tier, &key),
            crate::extract::resolve_cookie(&state, cookie_header.as_deref().unwrap_or_default()),
        );
        if info.count > info.limit {
            return rate_limit::rejected(tier, &info);
        }
        if let Ok(session) = session {
            req.extensions_mut()
                .insert(crate::extract::EarlySession(session));
        }

        // CSRF validation for state-changing methods — before the route so
        // ALL mutating endpoints (incl. /api/auth/login) require the token.
        if csrf::requires_token(req.method(), &path) && !csrf::validate(&req) {
            let mut res = csrf::rejected();
            // Set the cookie the client needs so a retry can succeed.
            csrf::apply(&mut res, &plan);
            return res;
        }

        let mut res = next.run(req).await;
        rate_limit::emit(tier, &info, &mut res);
        csrf::apply(&mut res, &plan);
        return res;
    }

    let mut res = next.run(req).await;
    csrf::apply(&mut res, &plan);
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use axum::http::{HeaderValue, StatusCode, header};
    use axum::routing::{get, post};
    use openshares_core::config::Config;
    use openshares_core::db::Db;
    use openshares_core::media::{CacheBackend, FsCache};
    use tower::ServiceExt;

    async fn ok() -> &'static str {
        "ok"
    }

    /// Full-stack app: tiny route surface + the real `apply` pipeline, on a
    /// tempdir SQLite (each test gets its own file DB — no serial needed).
    async fn test_app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("t.db").display()))
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let state = AppState {
            db,
            cache: CacheBackend::Fs(FsCache::new(dir.path().join("cache"))),
            config: Config {
                bot_token: None,
                app_url: "http://example.com".to_string(),
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
            jwt: None,
        };
        let router = Router::new()
            .route("/api/test", get(ok).post(ok))
            .route("/api/auth/check", get(ok))
            .route("/api/auth/login", post(ok))
            .route("/api/bot/webhook", post(ok))
            .route("/page", get(ok));
        (apply(router, state.clone()).with_state(state), dir)
    }

    fn req(method: &str, path: &str) -> Request {
        Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap()
    }

    fn hv<'r>(res: &'r Response, name: &str) -> Option<&'r HeaderValue> {
        res.headers().get(name)
    }

    #[tokio::test]
    async fn api_read_gets_full_decoration() {
        let (app, _dir) = test_app().await;
        let res = app.oneshot(req("GET", "/api/test")).await.unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        // correlation id generated + echoed
        let cid = hv(&res, "x-correlation-id").unwrap();
        assert_eq!(cid.to_str().unwrap().len(), 36);
        // security headers (spot-check the set)
        assert_eq!(hv(&res, "x-frame-options").unwrap(), "DENY");
        assert_eq!(
            hv(&res, "strict-transport-security").unwrap(),
            "max-age=63072000; includeSubDomains"
        );
        assert!(
            hv(&res, "content-security-policy")
                .unwrap()
                .to_str()
                .unwrap()
                .contains("default-src 'self'")
        );
        // XSRF cookie issued, SameSite=Strict, JS-readable (no HttpOnly)
        let cookie = hv(&res, "set-cookie").unwrap().to_str().unwrap();
        assert!(cookie.starts_with("XSRF-TOKEN="));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(!cookie.contains("HttpOnly"));
        // MW_READ tier headers
        assert_eq!(hv(&res, "x-ratelimit-limit").unwrap(), "600");
        assert!(hv(&res, "x-ratelimit-remaining").is_some());
        assert!(hv(&res, "x-ratelimit-reset").is_some());
        assert!(hv(&res, "x-bruteforce-protection").is_none());
    }

    #[tokio::test]
    async fn correlation_id_echoes_client_value() {
        let (app, _dir) = test_app().await;
        let res = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/test")
                    .header("x-correlation-id", "my-id-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(hv(&res, "x-correlation-id").unwrap(), "my-id-123");
    }

    #[tokio::test]
    async fn non_api_path_gets_headers_and_cookie_but_no_ratelimit() {
        let (app, _dir) = test_app().await;
        let res = app.oneshot(req("GET", "/page")).await.unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        assert!(hv(&res, "x-correlation-id").is_some());
        assert_eq!(hv(&res, "x-robots-tag").unwrap(), "noindex, nofollow");
        assert!(hv(&res, "set-cookie").is_some());
        // the /api-only pipeline never ran
        assert!(hv(&res, "x-ratelimit-limit").is_none());
    }

    #[tokio::test]
    async fn mutating_post_without_csrf_is_403() {
        let (app, _dir) = test_app().await;
        let res = app.oneshot(req("POST", "/api/test")).await.unwrap();

        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        // response carries the cookie for a retry — but, matching the TS
        // sibling branch, no rate-limit headers on this early return
        assert!(hv(&res, "set-cookie").is_some());
        assert!(hv(&res, "x-ratelimit-limit").is_none());
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "CSRF token missing or invalid.");
    }

    #[tokio::test]
    async fn mutating_post_with_csrf_passes() {
        let (app, _dir) = test_app().await;
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/test")
                    .header(header::COOKIE, "XSRF-TOKEN=tok123")
                    .header("x-csrf-token", "tok123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        // MW_WRITE tier
        assert_eq!(hv(&res, "x-ratelimit-limit").unwrap(), "120");
        // existing cookie is never rotated → no set-cookie on the response
        assert!(hv(&res, "set-cookie").is_none());
    }

    #[tokio::test]
    async fn webhook_is_csrf_exempt_and_webhook_tier() {
        let (app, _dir) = test_app().await;
        let res = app.oneshot(req("POST", "/api/bot/webhook")).await.unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(hv(&res, "x-ratelimit-limit").unwrap(), "240");
    }

    #[tokio::test]
    async fn options_on_api_short_circuits_to_204() {
        let (app, _dir) = test_app().await;
        let res = app.oneshot(req("OPTIONS", "/api/test")).await.unwrap();

        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        // still decorated by the outer layers, but no cookie / rate headers
        assert!(hv(&res, "x-correlation-id").is_some());
        assert_eq!(hv(&res, "x-content-type-options").unwrap(), "nosniff");
        assert!(hv(&res, "set-cookie").is_none());
        assert!(hv(&res, "x-ratelimit-limit").is_none());
    }

    #[tokio::test]
    async fn cross_origin_rejected_and_matching_origin_passes() {
        let (app, _dir) = test_app().await;
        let res = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/test")
                    .header(header::HOST, "example.com")
                    .header(header::ORIGIN, "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"Cross-origin request rejected.");

        let (app2, _d2) = test_app().await;
        let res2 = app2
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/test")
                    .header(header::HOST, "example.com")
                    .header(header::ORIGIN, "https://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res2.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_origin_is_bad_origin() {
        let (app, _dir) = test_app().await;
        let res = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/test")
                    .header(header::HOST, "example.com")
                    .header(header::ORIGIN, "not a url")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"Bad Origin header.");
    }

    #[tokio::test]
    async fn auth_tier_emits_bruteforce_header_and_429s_after_limit() {
        let (app, _dir) = test_app().await;

        // MW_AUTH tier on a GET → brute-force marker + 10/min limit
        let res = app
            .clone()
            .oneshot(req("GET", "/api/auth/check"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(hv(&res, "x-ratelimit-limit").unwrap(), "10");
        assert_eq!(
            hv(&res, "x-bruteforce-protection").unwrap(),
            "10-attempts-per-minute"
        );

        // burn the bucket: the GET above already consumed count=1, and each
        // CSRF-failed POST still consumes MW_AUTH count; counts 2..=10 pass,
        // so the 10th mutating hit overflows (count > limit) → 429 *before*
        // the CSRF check.
        for _ in 0..9 {
            let res = app
                .clone()
                .oneshot(req("POST", "/api/auth/login"))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN);
        }
        let res = app.oneshot(req("POST", "/api/auth/login")).await.unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(hv(&res, "x-ratelimit-limit").unwrap(), "10");
        assert_eq!(hv(&res, "x-ratelimit-remaining").unwrap(), "0");
        assert!(hv(&res, "x-ratelimit-reset").is_some());
        assert!(hv(&res, header::RETRY_AFTER.as_str()).is_some());
        // 429 early return: security headers present, no cookie, no
        // brute-force marker (that header only rides the pass-through).
        assert_eq!(hv(&res, "x-frame-options").unwrap(), "DENY");
        assert!(hv(&res, "set-cookie").is_none());
        assert!(hv(&res, "x-bruteforce-protection").is_none());
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["error"],
            "Too many login attempts. Please try again later."
        );
    }

    #[test]
    fn is_mutating_methods() {
        assert!(is_mutating(&Method::POST));
        assert!(is_mutating(&Method::PUT));
        assert!(is_mutating(&Method::PATCH));
        assert!(is_mutating(&Method::DELETE));
        assert!(!is_mutating(&Method::GET));
        assert!(!is_mutating(&Method::OPTIONS));
        assert!(!is_mutating(&Method::HEAD));
    }
}

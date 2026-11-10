//! Same-origin gate for `/api/*` — port of middleware.ts lines 84–102.
//!
//! Any request carrying an `Origin` header must have `origin.host ==
//! request host` (all methods — browsers omit Origin on same-origin GETs,
//! and the Telegram webhook sends none). Cross-origin preflights get a bare
//! `204` with no CORS headers, so the browser blocks the follow-up call
//! anyway.

use std::str::FromStr;

use axum::extract::Request;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

/// The Origin half of the gate. `Some(response)` = reject now; `None` = the
/// request may proceed. Order mirrors middleware.ts exactly: a malformed
/// Origin is `"Bad Origin header."`, a parseable-but-mismatched one is
/// `"Cross-origin request rejected."`.
pub fn check(req: &Request) -> Option<Response> {
    let origin = match req.headers().get(header::ORIGIN)?.to_str() {
        Ok(s) => s,
        // Non-ASCII bytes can never be a URL — same as `new URL` throwing.
        Err(_) => return Some(reject("Bad Origin header.")),
    };
    match origin_host(origin) {
        None => Some(reject("Bad Origin header.")),
        Some(host) if host != request_host(req) => Some(reject("Cross-origin request rejected.")),
        Some(_) => None,
    }
}

/// The bare `OPTIONS` → `204` short-circuit — deliberately no CORS surface.
pub fn options_response() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

fn reject(msg: &'static str) -> Response {
    (StatusCode::FORBIDDEN, msg).into_response()
}

/// `new URL(origin).host`: lowercase; default ports (`http:80`, `https:443`)
/// dropped, explicit ports kept; IPv6 literals keep their brackets —
/// `Some("[::1]:8080")`. `None` when the origin isn't an absolute
/// `scheme://host` URL (what `new URL` would throw on — e.g. `Origin: null`
/// sent by sandboxed frames).
fn origin_host(origin: &str) -> Option<String> {
    let uri = Uri::from_str(origin).ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    let raw = uri.host()?.to_ascii_lowercase();
    let host = if raw.contains(':') {
        format!("[{raw}]") // URL serializes IPv6 literals with brackets
    } else {
        raw
    };
    match uri.port_u16() {
        Some(p) if (scheme == "http" && p == 80) || (scheme == "https" && p == 443) => Some(host),
        Some(p) => Some(format!("{host}:{p}")),
        None => Some(host),
    }
}

/// `req.nextUrl.host` — the host the client originally addressed. Behind
/// a trusted proxy the public host arrives via `x-forwarded-host` (the
/// Next dev rewrite + TLS-terminating proxies rewrite `Host` to the
/// upstream). A browser can't inject `x-forwarded-host` — it isn't a
/// safelisted header, so it forces a preflight our `204`-without-CORS
/// already rejects — so trusting it here doesn't widen the CSRF surface.
/// Falls back to `Host`, then an absolute-form request URI.
fn request_host(req: &Request) -> String {
    let forwarded = req
        .headers()
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        // First hop names the public host in a proxy chain.
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if let Some(h) = forwarded {
        return h.to_ascii_lowercase();
    }
    if let Some(h) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        return h.trim().to_ascii_lowercase();
    }
    match (req.uri().host(), req.uri().port_u16()) {
        (Some(host), Some(p)) => format!("{}:{p}", host.to_ascii_lowercase()),
        (Some(host), None) => host.to_ascii_lowercase(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::HeaderValue;

    fn req_with(origin: Option<&str>, host: Option<&str>) -> Request {
        let mut b = Request::builder().method("GET").uri("/api/x");
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        if let Some(h) = host {
            b = b.header(header::HOST, h);
        }
        b.body(Body::empty()).unwrap()
    }

    #[test]
    fn origin_host_parsing() {
        assert_eq!(
            origin_host("https://example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            origin_host("https://example.com:8443").as_deref(),
            Some("example.com:8443")
        );
        // default ports drop, like URL.host
        assert_eq!(
            origin_host("https://example.com:443").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            origin_host("http://example.com:80").as_deref(),
            Some("example.com")
        );
        // uppercase normalizes
        assert_eq!(
            origin_host("HTTPS://EXAMPLE.COM/").as_deref(),
            Some("example.com")
        );
        // path/query are ignored — Origin is an origin
        assert_eq!(origin_host("https://a.b/c?d=e").as_deref(), Some("a.b"));
        // unparseable / not absolute → None → "Bad Origin header."
        assert!(origin_host("null").is_none());
        assert!(origin_host("not a url").is_none());
        assert!(origin_host("example.com").is_none()); // no scheme
    }

    #[test]
    fn check_no_origin_passes() {
        assert!(check(&req_with(None, Some("example.com"))).is_none());
        // no Host either — still passes; the Origin check only binds Origins
        assert!(check(&req_with(None, None)).is_none());
    }

    #[test]
    fn check_matching_and_mismatched_origins() {
        assert!(check(&req_with(Some("https://example.com"), Some("example.com"))).is_none());
        // explicit default port still matches the bare host
        assert!(
            check(&req_with(
                Some("https://example.com:443"),
                Some("example.com")
            ))
            .is_none()
        );
        // port is part of host — non-default port must appear on both sides
        assert!(
            check(&req_with(
                Some("http://example.com:8080"),
                Some("example.com:8080")
            ))
            .is_none()
        );
        assert!(
            check(&req_with(
                Some("http://example.com:8080"),
                Some("example.com")
            ))
            .is_some()
        );
        // different host → 403
        let res = check(&req_with(Some("https://evil.example"), Some("example.com"))).unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn check_bad_origin_message() {
        let res = check(&req_with(Some("bogus"), Some("example.com"))).unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn request_host_sources() {
        assert_eq!(
            request_host(&req_with(None, Some("Example.COM:3000"))),
            "example.com:3000"
        );
        assert_eq!(request_host(&req_with(None, None)), "");
    }

    #[test]
    fn request_host_prefers_forwarded() {
        let mut req = req_with(None, Some("internal:8080"));
        req.headers_mut().insert(
            "x-forwarded-host",
            HeaderValue::from_static("Public.Example:3000"),
        );
        assert_eq!(request_host(&req), "public.example:3000");

        // proxy chain → first hop is the public host
        req.headers_mut().insert(
            "x-forwarded-host",
            HeaderValue::from_static("a.example, internal:8080"),
        );
        assert_eq!(request_host(&req), "a.example");

        // empty/absent falls back to Host
        req.headers_mut()
            .insert("x-forwarded-host", HeaderValue::from_static("  "));
        assert_eq!(request_host(&req), "internal:8080");
    }

    #[test]
    fn check_origin_matches_forwarded_host() {
        // The :3000 dev rewrite: browser Origin is the dev origin, Host was
        // rewritten to the upstream, x-forwarded-host carries the public host.
        let mut req = req_with(Some("http://localhost:3000"), Some("127.0.0.1:8080"));
        req.headers_mut().insert(
            "x-forwarded-host",
            HeaderValue::from_static("localhost:3000"),
        );
        assert!(check(&req).is_none());
        // and a genuinely foreign origin still 403s through the same path
        let mut req = req_with(Some("https://evil.example"), Some("127.0.0.1:8080"));
        req.headers_mut().insert(
            "x-forwarded-host",
            HeaderValue::from_static("localhost:3000"),
        );
        assert!(check(&req).is_some());
    }
}

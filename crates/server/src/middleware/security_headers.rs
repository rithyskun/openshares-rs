//! `SECURITY_HEADERS` + `csp()` — port of middleware.ts lines 4–37 and the
//! header half of `setSecurity` (`res.headers.set` → overwrite semantics).
//!
//! The CSP is built once in [`apply`](super::apply) — the layer state is the
//! finished `content-security-policy` value, not recomputed per request.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

/// The verbatim `SECURITY_HEADERS` table from middleware.ts (`&str` keys —
/// `HeaderName` has interior mutability, so it can't live in a `const`).
const HEADERS: &[(&str, &str)] = &[
    (
        "strict-transport-security",
        "max-age=63072000; includeSubDomains",
    ),
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "strict-origin-when-cross-origin"),
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=()",
    ),
    ("x-permitted-cross-domain-policies", "none"),
    // private vault — keep every page (incl. public share links) out of indexes
    ("x-robots-tag", "noindex, nofollow"),
    // Cross-origin isolation: our responses may only be embedded/fetched
    // same-origin, and we embed nothing cross-origin ourselves.
    ("cross-origin-resource-policy", "same-origin"),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-embedder-policy", "require-corp"),
];

/// Middle layer — stamps `SECURITY_HEADERS` + CSP on every response,
/// including the gateway's early returns (the `setSecurity` calls in
/// middleware.ts return paths).
pub async fn set(State(csp): State<Arc<str>>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    apply(&mut res, &csp);
    res
}

/// `setSecurity`'s header half — every entry overwrites what the route set
/// (`headers.set`, not append).
pub fn apply(res: &mut Response, csp: &str) {
    let h = res.headers_mut();
    for &(name, value) in HEADERS {
        h.insert(name, HeaderValue::from_static(value));
    }
    // CSP chars are all visible ASCII — unparseable means a bug, and a
    // missing CSP header is a worse failure mode than skipping it loudly.
    match HeaderValue::from_str(csp) {
        Ok(v) => {
            h.insert(header::CONTENT_SECURITY_POLICY, v);
        }
        Err(e) => tracing::error!(error = %e, "generated CSP is not a valid header value"),
    }
}

/// `csp()` in middleware.ts — `dev` additionally allows `'unsafe-eval'` in
/// `script-src` (Next dev-mode HMR needs it).
pub fn csp(dev: bool) -> String {
    let eval = if dev { " 'unsafe-eval'" } else { "" };
    [
        "default-src 'self'".to_string(),
        format!("script-src 'self' 'unsafe-inline'{eval}"),
        "style-src 'self' 'unsafe-inline'".to_string(),
        "img-src 'self' data: blob:".to_string(),
        "media-src 'self' blob:".to_string(),
        "connect-src 'self'".to_string(),
        "font-src 'self' data:".to_string(),
        "object-src 'none'".to_string(),
        "frame-src 'none'".to_string(),
        "base-uri 'self'".to_string(),
        "form-action 'self'".to_string(),
        "frame-ancestors 'none'".to_string(),
    ]
    .join("; ")
}

/// `process.env.NODE_ENV !== "production"` — honors a real `NODE_ENV` env
/// var when set; otherwise debug builds count as dev and release builds as
/// production (this binary isn't run by Node, so the env var only exists if
/// the operator sets it deliberately).
pub fn dev_mode() -> bool {
    match std::env::var("NODE_ENV") {
        Ok(v) => v.trim() != "production",
        Err(_) => cfg!(debug_assertions),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csp_dev_adds_unsafe_eval() {
        let prod = csp(false);
        let dev = csp(true);
        assert!(prod.contains("script-src 'self' 'unsafe-inline'"));
        assert!(!prod.contains("unsafe-eval"));
        assert!(dev.contains("script-src 'self' 'unsafe-inline' 'unsafe-eval'"));
        // the rest of the policy is identical
        for directive in [
            "default-src 'self'",
            "object-src 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(prod.contains(directive));
            assert!(dev.contains(directive));
        }
    }

    #[test]
    fn header_table_matches_ts() {
        // 10 fixed entries + CSP applied separately = the 11 headers TS sets.
        assert_eq!(HEADERS.len(), 10);
        let names: Vec<&str> = HEADERS.iter().map(|(n, _)| *n).collect();
        for expected in [
            "strict-transport-security",
            "x-content-type-options",
            "x-frame-options",
            "referrer-policy",
            "permissions-policy",
            "x-permitted-cross-domain-policies",
            "x-robots-tag",
            "cross-origin-resource-policy",
            "cross-origin-opener-policy",
            "cross-origin-embedder-policy",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }
}

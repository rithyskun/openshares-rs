//! Phase-1 IP rate limit — port of middleware.ts lines 48–56 and 104–126.
//!
//! Anonymous, pre-auth, IP-keyed flood protection (post-auth per-user
//! limits live inside routes via `rate_limited`). The `MW_*` tiers map onto
//! [`Limiter`] 1:1, so buckets share the `rate_limits` table namespace the
//! TS app writes. `rate_limit_info` already fails open on DB errors — no
//! second layer is added here.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::Request;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use openshares_core::crypto::security::{Limiter, RateLimitInfo, client_ip};
use openshares_core::db::now;

use crate::extract::ClientIp;

const LIMIT: HeaderName = HeaderName::from_static("x-ratelimit-limit");
const REMAINING: HeaderName = HeaderName::from_static("x-ratelimit-remaining");
const RESET: HeaderName = HeaderName::from_static("x-ratelimit-reset");
const BRUTEFORCE: HeaderName = HeaderName::from_static("x-bruteforce-protection");
const CF_CONNECTING_IP: HeaderName = HeaderName::from_static("cf-connecting-ip");
const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// `mwTier(pathname, method)` — prefix tiers first, then method class.
pub fn mw_tier(path: &str, method: &Method) -> Limiter {
    if path.starts_with("/api/auth/") {
        return Limiter::MwAuth;
    }
    if path.starts_with("/api/bot/webhook") {
        return Limiter::MwWebhook;
    }
    if super::is_mutating(method) {
        Limiter::MwWrite
    } else {
        Limiter::MwRead
    }
}

/// Resolve the client IP for rate-limit keying. With `TRUST_PROXY`
/// (a real reverse proxy in front — cloudflared, nginx), the forwarding
/// headers are proxy-set: `cf-connecting-ip` wins, else the first
/// `x-forwarded-for` hop ([`client_ip`]). Without it those headers are
/// client-spoofable — an attacker could rotate them for a fresh bucket
/// per request — so the TCP peer address (`ConnectInfo`, set by
/// `into_make_service_with_connect_info`) is the only safe source.
/// `"unknown"` when nothing trustworthy exists.
pub fn resolve_ip(req: &Request, trust_proxy: bool) -> String {
    if trust_proxy {
        let cf = req.headers().get(&CF_CONNECTING_IP).and_then(str_value);
        let xff = req.headers().get(&X_FORWARDED_FOR).and_then(str_value);
        let forwarded = client_ip(cf, xff);
        if !forwarded.is_empty() && forwarded != "unknown" {
            return forwarded;
        }
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// `` `ip:${clientIp(req)}` `` — reads the [`ClientIp`] extension the
/// gateway resolved via [`resolve_ip`]; `"unknown"` if absent (unit
/// tests, or a path the gateway never saw).
pub fn ip_key(req: &Request) -> String {
    let ip = req
        .extensions()
        .get::<ClientIp>()
        .map(|c| c.0.as_str())
        .unwrap_or("unknown");
    format!("ip:{ip}")
}

/// The 429 early return — middleware.ts lines 109–126: JSON error body,
/// `Retry-After` until window reset, `x-ratelimit-*` with remaining forced
/// to `0`. `MW_AUTH` buckets get the login-specific message. (No
/// `x-bruteforce-protection` here — the TS only emits it on pass-through.)
pub fn rejected(tier: Limiter, info: &RateLimitInfo) -> Response {
    let error = if tier == Limiter::MwAuth {
        "Too many login attempts. Please try again later."
    } else {
        "Too many requests — slow down and retry shortly."
    };
    let retry_after = std::cmp::max(1, info.reset - now());
    let mut res = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "error": error })),
    )
        .into_response();
    let h = res.headers_mut();
    h.insert(header::RETRY_AFTER, num(retry_after));
    h.insert(LIMIT, num(info.limit));
    h.insert(REMAINING, HeaderValue::from_static("0"));
    h.insert(RESET, num(info.reset));
    res
}

/// `x-ratelimit-*` on the pass-through response, plus the auth tier's
/// scanner-visible `x-bruteforce-protection` marker (middleware.ts lines
/// 143–150 — "signal that brute-force protection exists even before the
/// 429 fires").
pub fn emit(tier: Limiter, info: &RateLimitInfo, res: &mut Response) {
    let remaining = std::cmp::max(0, info.limit - info.count);
    let h = res.headers_mut();
    h.insert(LIMIT, num(info.limit));
    h.insert(REMAINING, num(remaining));
    h.insert(RESET, num(info.reset));
    if tier == Limiter::MwAuth {
        h.insert(
            BRUTEFORCE,
            header_value(&format!("{}-attempts-per-minute", info.limit)),
        );
    }
}

fn str_value(v: &HeaderValue) -> Option<&str> {
    v.to_str().ok()
}

/// i64 → `HeaderValue`. Digit-only content can never fail to parse; the
/// fallback keeps the signature total without an `unwrap`.
fn num(v: i64) -> HeaderValue {
    header_value(&v.to_string())
}

fn header_value(v: &str) -> HeaderValue {
    HeaderValue::from_str(v).unwrap_or_else(|_| HeaderValue::from_static("0"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::request::Builder;

    fn req() -> Builder {
        Request::builder().method("GET").uri("/api/x")
    }

    #[test]
    fn tier_classification() {
        // path prefixes win over method
        assert_eq!(mw_tier("/api/auth/login", &Method::POST), Limiter::MwAuth);
        assert_eq!(mw_tier("/api/auth/telegram", &Method::GET), Limiter::MwAuth);
        assert_eq!(
            mw_tier("/api/bot/webhook", &Method::POST),
            Limiter::MwWebhook
        );
        // startsWith semantics — /api/bot/webhookXYZ is still webhook tier
        assert_eq!(
            mw_tier("/api/bot/webhook-extra", &Method::POST),
            Limiter::MwWebhook
        );
        // method class for everything else
        assert_eq!(mw_tier("/api/media", &Method::GET), Limiter::MwRead);
        assert_eq!(mw_tier("/api/media", &Method::POST), Limiter::MwWrite);
        assert_eq!(mw_tier("/api/media", &Method::DELETE), Limiter::MwWrite);
        assert_eq!(mw_tier("/api/media", &Method::OPTIONS), Limiter::MwRead);
        // the MwAuth prefix needs the trailing slash — "/api/authent" is not auth
        assert_eq!(mw_tier("/api/authent", &Method::GET), Limiter::MwRead);
    }

    fn peer() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([7, 7, 7, 7], 4321)))
    }

    #[test]
    fn resolve_ip_proxy_headers_only_when_trusted() {
        // trust_proxy → cf-connecting-ip wins over XFF
        let r = req()
            .header(&CF_CONNECTING_IP, "1.2.3.4")
            .header(&X_FORWARDED_FOR, "9.9.9.9, 8.8.8.8")
            .extension(peer())
            .body(Body::empty())
            .unwrap();
        assert_eq!(resolve_ip(&r, true), "1.2.3.4");
        // first XFF hop
        let r = req()
            .header(&X_FORWARDED_FOR, "  5.6.7.8 , 9.9.9.9")
            .extension(peer())
            .body(Body::empty())
            .unwrap();
        assert_eq!(resolve_ip(&r, true), "5.6.7.8");
        // no forwarding headers → socket peer, not "unknown"
        let r = req().extension(peer()).body(Body::empty()).unwrap();
        assert_eq!(resolve_ip(&r, true), "7.7.7.7");
    }

    #[test]
    fn resolve_ip_without_proxy_ignores_spoofable_headers() {
        // Attacker-set cf-connecting-ip must NOT earn a private bucket.
        let r = req()
            .header(&CF_CONNECTING_IP, "1.2.3.4")
            .header(&X_FORWARDED_FOR, "5.6.7.8")
            .extension(peer())
            .body(Body::empty())
            .unwrap();
        assert_eq!(resolve_ip(&r, false), "7.7.7.7");
        // no ConnectInfo (tests, non-TCP transports) → "unknown"
        let r = req()
            .header(&CF_CONNECTING_IP, "1.2.3.4")
            .body(Body::empty())
            .unwrap();
        assert_eq!(resolve_ip(&r, false), "unknown");
    }

    #[test]
    fn ip_key_reads_gateway_extension() {
        let r = req()
            .extension(ClientIp("9.9.9.9".to_string()))
            .body(Body::empty())
            .unwrap();
        assert_eq!(ip_key(&r), "ip:9.9.9.9");
        let r = req().body(Body::empty()).unwrap();
        assert_eq!(ip_key(&r), "ip:unknown");
    }

    #[test]
    fn emit_headers_and_bruteforce() {
        let info = RateLimitInfo {
            count: 3,
            limit: 10,
            reset: 1_700_000_060,
        };
        let mut res = StatusCode::OK.into_response();
        emit(Limiter::MwAuth, &info, &mut res);
        let h = res.headers();
        assert_eq!(h.get(&LIMIT).unwrap(), "10");
        assert_eq!(h.get(&REMAINING).unwrap(), "7");
        assert_eq!(h.get(&RESET).unwrap(), "1700000060");
        assert_eq!(h.get(&BRUTEFORCE).unwrap(), "10-attempts-per-minute");

        // non-auth tier → no brute-force marker
        let mut res2 = StatusCode::OK.into_response();
        emit(Limiter::MwRead, &info, &mut res2);
        assert!(res2.headers().get(&BRUTEFORCE).is_none());

        // remaining clamps at 0
        let over = RateLimitInfo {
            count: 12,
            limit: 10,
            reset: 1_700_000_060,
        };
        let mut res3 = StatusCode::OK.into_response();
        emit(Limiter::MwRead, &over, &mut res3);
        assert_eq!(res3.headers().get(&REMAINING).unwrap(), "0");
    }

    #[tokio::test]
    async fn rejected_429_shape() {
        let info = RateLimitInfo {
            count: 11,
            limit: 10,
            reset: now() + 30,
        };
        let res = rejected(Limiter::MwAuth, &info);
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(res.headers().get(&LIMIT).unwrap(), "10");
        assert_eq!(res.headers().get(&REMAINING).unwrap(), "0");
        assert!(res.headers().get(header::RETRY_AFTER).is_some());
        assert!(res.headers().get(&BRUTEFORCE).is_none());
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body["error"],
            "Too many login attempts. Please try again later."
        );

        let res2 = rejected(Limiter::MwRead, &info);
        let bytes2 = axum::body::to_bytes(res2.into_body(), usize::MAX)
            .await
            .unwrap();
        let body2: serde_json::Value = serde_json::from_slice(&bytes2).unwrap();
        assert_eq!(
            body2["error"],
            "Too many requests — slow down and retry shortly."
        );
    }
}

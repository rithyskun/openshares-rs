//! `x-correlation-id` — port of middleware.ts line 80 and the id half of
//! `setSecurity`.
//!
//! The id comes from the incoming `x-correlation-id` header when present
//! (echoed verbatim — the TS `??` uses any present value, even empty), else
//! a uuid-v4-style id is generated (`crypto.randomUUID()` in TS — here from
//! `rand`, no uuid crate). It's stashed in request extensions for handlers
//! and future log spans, and stamped on every response.

use std::fmt::Write as _;

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use rand::RngExt;

/// Header carrying the request-scoped id — `x-correlation-id` in middleware.ts.
pub const HEADER_NAME: HeaderName = HeaderName::from_static("x-correlation-id");

/// Request extension holding the resolved id. The TS app only echoes it on
/// the response; keeping it on the request lets handlers/tracing pick it up.
#[derive(Debug, Clone)]
pub struct CorrelationId(pub String);

/// Outermost layer — resolves the id before any inner layer can
/// short-circuit, then stamps it on whatever response comes back. That
/// mirrors `setSecurity(res, correlationId)` being applied to the TS early
/// returns (bad origin, 204, 429, CSRF-403) as well as pass-throughs.
pub async fn set(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(uuid_v4);
    req.extensions_mut().insert(CorrelationId(id.clone()));

    let mut res = next.run(req).await;
    res.headers_mut().insert(HEADER_NAME, header_value(&id));
    res
}

/// `crypto.randomUUID()` — RFC 4122 v4 layout from 16 random bytes.
fn uuid_v4() -> String {
    let mut b: [u8; 16] = rand::rng().random();
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10xx
    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        let _ = write!(s, "{byte:02x}");
    }
    s
}

fn header_value(v: &str) -> HeaderValue {
    // Client-supplied ids are already valid `HeaderValue`s (we read them via
    // `to_str`); generated ids are pure ASCII hex + dashes. The fallback is
    // unreachable — it's only here to keep this infallible-looking.
    HeaderValue::from_str(v).unwrap_or_else(|_| HeaderValue::from_static("unknown"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_v4_shape() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[8..9], "-");
        assert_eq!(&id[13..14], "-");
        assert_eq!(&id[18..19], "-");
        assert_eq!(&id[23..24], "-");
        assert_eq!(&id[14..15], "4"); // version nibble
        assert!("89ab".contains(&id[19..20])); // variant nibble
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'));
    }

    #[test]
    fn header_value_fallback_never_panics() {
        assert_eq!(header_value("abc-123").to_str().unwrap(), "abc-123");
    }
}

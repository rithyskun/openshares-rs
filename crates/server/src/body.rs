//! Request body extraction — the axum half of `verifySignedBody` +
//! `parseJsonBody` in `src/lib/signing.ts`.
//!
//! Every mutating `/api/*` handler that consumes JSON should read its
//! body through [`json_body`]: it pulls the raw bytes, verifies the
//! HMAC signature when [`requires_signature`] says the path needs one
//! (auth/bot/multipart are exempt inside), then tolerantly parses —
//! mirroring `req.json().catch(() => ({}))`.

use axum::extract::Request;
use axum::http::StatusCode;
use serde::de::DeserializeOwned;

use openshares_core::crypto::signing::{
    HEADER_NONCE, HEADER_SIGNATURE, HEADER_TS, SignatureInput, SignatureVerdict, parse_json_body,
    requires_signature, verify_signed_body,
};

use crate::error::{ApiError, Result};

/// Body cap for mutating API calls — matches axum's default
/// `DefaultBodyLimit` so behavior doesn't change with the call style.
const JSON_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Read + verify + parse a possibly-signed JSON body.
///
/// `sid` is the caller's session id — `Some(user.sid)` for
/// session-gated routes, `None` for anonymous; `verify_signed_body`
/// passes through when signing is disabled or no sid exists
/// (defense-in-depth, never an auth check). The signature is computed
/// over `req.uri().path()` internally.
///
/// `Err` is a `403` with the exact TS message (`stale` vs other verdicts).
pub async fn json_body<T: DeserializeOwned + Default>(
    req: Request,
    sid: Option<&str>,
) -> Result<T> {
    Ok(parse_json_body(&raw_body(req, sid).await?))
}

/// The verified raw body string — for handlers that need the text
/// before deciding how to parse it (the TS `verifySignedBody` return).
pub async fn raw_body(req: Request, sid: Option<&str>) -> Result<String> {
    let method = req.method().as_str().to_string();
    let pathname = req.uri().path().to_string();
    let headers = SignatureInput {
        signature: header_str(&req, HEADER_SIGNATURE),
        ts: header_str(&req, HEADER_TS),
        nonce: header_str(&req, HEADER_NONCE),
    };

    let bytes = axum::body::to_bytes(req.into_body(), JSON_BODY_LIMIT)
        .await
        .map_err(|e| ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, e.to_string()))?;
    let raw = String::from_utf8(bytes.to_vec())
        .map_err(|_| ApiError::bad_request("Body is not valid UTF-8."))?;

    if !requires_signature(&method, &pathname) {
        return Ok(raw);
    }
    verify_signed_body(raw, &method, &pathname, &headers, sid).map_err(verdict_error)
}

fn verdict_error(verdict: SignatureVerdict) -> ApiError {
    let message = match verdict {
        SignatureVerdict::Stale => "Request signature expired — retry.",
        _ => "Request signature missing or invalid.",
    };
    ApiError::new(StatusCode::FORBIDDEN, message)
}

fn header_str(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

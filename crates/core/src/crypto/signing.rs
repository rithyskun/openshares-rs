//! HMAC-SHA256 request signing for mutating API calls — port of
//! `src/lib/signing.ts`.
//!
//! The per-session signing key is derived statelessly as
//! `base64(HMAC-SHA256(REQUEST_SIGNING_SECRET, "signing:" + sid))` — any
//! request carrying a valid `os_session` cookie can be checked by
//! recomputing the key from the session id. Wire format (same as TS):
//!
//! ```text
//! sig = HMAC-SHA256(key, method + "\n" + path + "\n" + sha256hex(body)
//!                    + "\n" + ts + "\n" + nonce)
//! ```
//!
//! sent as `x-signature` / `x-signature-ts` / `x-signature-nonce`.
//! Timestamps more than ±5 min off are rejected, bounding the replay
//! window. Signing is defense-in-depth, never an auth check — when
//! `REQUEST_SIGNING_SECRET` is unset everything passes through.

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::crypto::{env, hex_encode, now_ms};
use crate::error::{AppError, Result};

/// `SIGN_MAX_AGE_MS` — replay window.
pub const SIGN_MAX_AGE_MS: i64 = 5 * 60 * 1000;

/// Header names — same as the TS app emits.
pub const HEADER_SIGNATURE: &str = "x-signature";
pub const HEADER_TS: &str = "x-signature-ts";
pub const HEADER_NONCE: &str = "x-signature-nonce";

const MUTATING: [&str; 4] = ["POST", "PUT", "PATCH", "DELETE"];

/// `SignatureHeaders` — values for the three `x-signature*` headers.
#[derive(Debug, Clone)]
pub struct SignatureHeaders {
    pub signature: String,
    pub ts: String,
    pub nonce: String,
}

/// `SignatureInput` — the three request headers, `None` when absent.
#[derive(Debug, Clone, Default)]
pub struct SignatureInput {
    pub signature: Option<String>,
    pub ts: Option<String>,
    pub nonce: Option<String>,
}

/// `SignatureVerdict` — `verifySignature` outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureVerdict {
    Ok,
    Missing,
    Stale,
    Bad,
}

/// `SIGN_EXEMPT` — paths that are never signed: pre-session auth endpoints
/// (no key exists yet), server-to-server bot traffic, and multipart uploads
/// (the XHR/FormData boundary isn't knowable before send, so the wire bytes
/// can't be hashed). Same rules as the TS regexes
/// `/^\/api\/auth\//`, `/^\/api\/bot\//`, `/^\/api\/albums\/[^/]+\/files$/`.
fn is_sign_exempt(path: &str) -> bool {
    if path.starts_with("/api/auth/") || path.starts_with("/api/bot/") {
        return true;
    }
    if let Some(rest) = path.strip_prefix("/api/albums/")
        && let Some(slug) = rest.strip_suffix("/files")
    {
        return !slug.is_empty() && !slug.contains('/');
    }
    false
}

/// `requiresSignature` — mutating `/api/*` calls minus the exempt set.
/// The query string is stripped before matching, like `pathname.split("?")[0]`.
pub fn requires_signature(method: &str, pathname: &str) -> bool {
    if !MUTATING.contains(&method.to_uppercase().as_str()) {
        return false;
    }
    let path = pathname.split('?').next().unwrap_or(pathname);
    if !path.starts_with("/api/") {
        return false;
    }
    !is_sign_exempt(path)
}

fn b64(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
}

fn b64decode(s: &str) -> Option<Vec<u8>> {
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s).ok()
}

/// `hmacSha256` — HMAC accepts keys of any length, so `new_from_slice` is
/// infallible in practice; a failure logs and yields an empty mac, which can
/// never verify (fails closed).
fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = match <Hmac<Sha256> as KeyInit>::new_from_slice(key) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "hmac init failed");
            return Vec::new();
        }
    };
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// `sha256Hex` — lowercase hex digest of the UTF-8 body.
fn sha256_hex(data: &str) -> String {
    hex_encode(&Sha256::digest(data.as_bytes()))
}

/// `canonicalMessage`.
fn canonical_message(method: &str, path: &str, body_hash: &str, ts: &str, nonce: &str) -> String {
    format!(
        "{}\n{path}\n{body_hash}\n{ts}\n{nonce}",
        method.to_uppercase()
    )
}

/// `crypto.randomUUID()` — uuid v4 (nonce value; the wire format, not the
/// generation, matters for interop).
fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut b);
    b[6] = b[6] & 0x0f | 0x40; // version 4
    b[8] = b[8] & 0x3f | 0x80; // variant 10xx
    let h = hex_encode(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// `signRequest` — client side: signature headers for one request.
pub fn sign_request(
    signing_key_b64: &str,
    method: &str,
    pathname: &str,
    body: &str,
) -> Result<SignatureHeaders> {
    let key = b64decode(signing_key_b64)
        .ok_or_else(|| AppError::Crypto("signing key is not valid base64".to_string()))?;
    let ts = now_ms().to_string();
    let nonce = uuid_v4();
    let body_hash = sha256_hex(body);
    let sig = hmac_sha256(
        &key,
        canonical_message(method, pathname, &body_hash, &ts, &nonce).as_bytes(),
    );
    Ok(SignatureHeaders {
        signature: b64(&sig),
        ts,
        nonce,
    })
}

/// `deriveSigningKey` — stateless per-session key, no table.
pub fn derive_signing_key(secret: &str, sid: &str) -> String {
    b64(&hmac_sha256(
        secret.as_bytes(),
        format!("signing:{sid}").as_bytes(),
    ))
}

/// `signingEnabled` — active only when `REQUEST_SIGNING_SECRET` is set.
pub fn signing_enabled() -> bool {
    env("REQUEST_SIGNING_SECRET").is_some()
}

/// `signingKeyFor` — key to hand the client for session `sid`, or `None`
/// when signing is off.
pub fn signing_key_for(sid: &str) -> Option<String> {
    env("REQUEST_SIGNING_SECRET").map(|secret| derive_signing_key(&secret, sid))
}

/// `verifySignature` — server side: check the signature headers against the
/// raw request body.
pub fn verify_signature(
    headers: &SignatureInput,
    signing_key_b64: &str,
    method: &str,
    pathname: &str,
    raw_body: &str,
) -> SignatureVerdict {
    let (Some(signature), Some(ts), Some(nonce)) =
        (&headers.signature, &headers.ts, &headers.nonce)
    else {
        return SignatureVerdict::Missing;
    };
    // Number(ts) — garbage or non-finite → stale.
    let Ok(ts_num) = ts.trim().parse::<f64>() else {
        return SignatureVerdict::Stale;
    };
    if !ts_num.is_finite() || (now_ms() as f64 - ts_num).abs() > SIGN_MAX_AGE_MS as f64 {
        return SignatureVerdict::Stale;
    }
    let body_hash = sha256_hex(raw_body);
    let Some(key) = b64decode(signing_key_b64) else {
        // TS would throw inside importKey (→ 500); we fail closed instead.
        return SignatureVerdict::Bad;
    };
    let expected = hmac_sha256(
        &key,
        canonical_message(method, pathname, &body_hash, ts, nonce).as_bytes(),
    );
    let Some(given) = b64decode(signature) else {
        return SignatureVerdict::Bad;
    };
    // safeEq — constant-time, length-checked.
    if given[..].ct_eq(&expected[..]).into() {
        SignatureVerdict::Ok
    } else {
        SignatureVerdict::Bad
    }
}

/// `verifySignedBody` — server-side guard for mutating handlers. Reads the
/// body once (the server already consumed the stream), verifies it against
/// the caller's session-derived key.
///
/// Returns `Ok(raw)` when the body may be used — signature ok, or signing
/// disabled (`REQUEST_SIGNING_SECRET` unset / no `sid`). `Err(verdict)`
/// means reject with 403 (stale → "expired — retry", else "missing or
/// invalid").
pub fn verify_signed_body(
    raw: String,
    method: &str,
    pathname: &str,
    headers: &SignatureInput,
    sid: Option<&str>,
) -> std::result::Result<String, SignatureVerdict> {
    let Some(secret) = env("REQUEST_SIGNING_SECRET") else {
        return Ok(raw);
    };
    let Some(sid) = sid else {
        return Ok(raw);
    };
    let key = derive_signing_key(&secret, sid);
    match verify_signature(headers, &key, method, pathname, &raw) {
        SignatureVerdict::Ok => Ok(raw),
        verdict => Err(verdict),
    }
}

/// `parseJsonBody` — `JSON.parse` that tolerates empty/garbage bodies,
/// mirroring `req.json().catch(() => ({}))`.
pub fn parse_json_body<T: serde::de::DeserializeOwned + Default>(raw: &str) -> T {
    if raw.is_empty() {
        return T::default();
    }
    serde_json::from_str(raw).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(sig: Option<String>, ts: Option<String>, nonce: Option<String>) -> SignatureInput {
        SignatureInput {
            signature: sig,
            ts,
            nonce,
        }
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let key = derive_signing_key("secret", "sid-1");
        let h = sign_request(&key, "post", "/api/media/1/caption", r#"{"a":1}"#).unwrap();
        let headers = input(Some(h.signature), Some(h.ts), Some(h.nonce));
        assert_eq!(
            verify_signature(&headers, &key, "POST", "/api/media/1/caption", r#"{"a":1}"#),
            SignatureVerdict::Ok
        );
    }

    #[test]
    fn hmac_matches_rfc4231() {
        // RFC 4231 test case 2: key "Jefe", msg "what do ya want for nothing?"
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex_encode(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn verify_verdicts() {
        let key = derive_signing_key("secret", "sid-1");
        let good = sign_request(&key, "POST", "/api/x", "body").unwrap();

        // missing
        assert_eq!(
            verify_signature(&input(None, None, None), &key, "POST", "/api/x", "body"),
            SignatureVerdict::Missing
        );
        assert_eq!(
            verify_signature(
                &input(Some(good.signature.clone()), None, Some(good.nonce.clone())),
                &key,
                "POST",
                "/api/x",
                "body"
            ),
            SignatureVerdict::Missing
        );

        // stale
        let stale_ts = (now_ms() - SIGN_MAX_AGE_MS - 1).to_string();
        assert_eq!(
            verify_signature(
                &input(
                    Some(good.signature.clone()),
                    Some(stale_ts),
                    Some(good.nonce.clone())
                ),
                &key,
                "POST",
                "/api/x",
                "body"
            ),
            SignatureVerdict::Stale
        );
        assert_eq!(
            verify_signature(
                &input(
                    Some(good.signature.clone()),
                    Some("not-a-number".to_string()),
                    Some(good.nonce.clone())
                ),
                &key,
                "POST",
                "/api/x",
                "body"
            ),
            SignatureVerdict::Stale
        );

        // bad — tampered body / wrong path / wrong key / garbage signature
        let headers = input(
            Some(good.signature.clone()),
            Some(good.ts.clone()),
            Some(good.nonce.clone()),
        );
        assert_eq!(
            verify_signature(&headers, &key, "POST", "/api/x", "tampered"),
            SignatureVerdict::Bad
        );
        assert_eq!(
            verify_signature(&headers, &key, "POST", "/api/other", "body"),
            SignatureVerdict::Bad
        );
        let other_key = derive_signing_key("secret", "sid-2");
        assert_eq!(
            verify_signature(&headers, &other_key, "POST", "/api/x", "body"),
            SignatureVerdict::Bad
        );
        assert_eq!(
            verify_signature(
                &input(
                    Some("!!!not-b64".to_string()),
                    Some(good.ts.clone()),
                    Some(good.nonce.clone())
                ),
                &key,
                "POST",
                "/api/x",
                "body"
            ),
            SignatureVerdict::Bad
        );
    }

    #[test]
    fn requires_signature_rules() {
        assert!(requires_signature("POST", "/api/media/1"));
        assert!(requires_signature("delete", "/api/media/1?x=1"));
        assert!(!requires_signature("GET", "/api/media/1"));
        assert!(!requires_signature("POST", "/login")); // not /api/*
        // exempt
        assert!(!requires_signature("POST", "/api/auth/login"));
        assert!(!requires_signature("POST", "/api/bot/webhook"));
        assert!(!requires_signature("GET", "/api/auth/me")); // GET is never signed anyway
        assert!(!requires_signature("POST", "/api/albums/abc/files"));
        assert!(requires_signature("POST", "/api/albums/abc/files/extra")); // suffix doesn't match
        assert!(requires_signature("POST", "/api/albums//files")); // empty slug
        assert!(requires_signature("POST", "/api/albums/a/b/files")); // slash in slug
    }

    #[test]
    fn signed_body_passthrough_when_unsigned() {
        // sid absent → pass through regardless of env (fail-open posture)
        let raw = "{}".to_string();
        assert_eq!(
            verify_signed_body(
                raw.clone(),
                "POST",
                "/api/x",
                &SignatureInput::default(),
                None
            )
            .unwrap(),
            raw
        );
    }

    #[test]
    fn parse_body_tolerant() {
        use std::collections::HashMap;
        let v: HashMap<String, i64> = parse_json_body(r#"{"a":1}"#);
        assert_eq!(v.get("a"), Some(&1));
        let empty: HashMap<String, i64> = parse_json_body("");
        assert!(empty.is_empty());
        let garbage: HashMap<String, i64> = parse_json_body("{not json");
        assert!(garbage.is_empty());
    }
}

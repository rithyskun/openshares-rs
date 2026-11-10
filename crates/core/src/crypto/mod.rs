//! Cryptographic primitives — ports of `src/lib/{auth,jwt,envelope,signing,security}.ts`.
//!
//! Phase 6 scope (hardest, most security-critical — port last):
//!
//! - `password` — scrypt hash/verify (`scrypt` crate; node:crypto scrypt params)
//!   + `sessions` table queries (`auth.ts`)
//! - `jwt`      — RS256 session JWTs (`jsonwebtoken`; `jwt.ts`)
//! - `envelope` — RSA-OAEP + AES-256-GCM credential envelope (`rsa`, `aes-gcm`;
//!   `envelope.ts`)
//! - `signing`  — HMAC-SHA256 request signing (`hmac`, `sha2`; `signing.ts`)
//! - `security` — constant-time compare, client IP, rate-limit helpers
//!   (`security.ts`)
//!
//! CPU-bound ops (scrypt, RSA decrypt) expose async wrappers that run via
//! `tokio::task::spawn_blocking` — never block the async runtime.

pub mod envelope;
pub mod jwt;
pub mod password;
pub mod security;
pub mod signing;

/// `env()` from `src/lib/env.ts` — trimmed-or-undefined env read.
pub(crate) fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `Date.now()` — unix epoch milliseconds.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Lowercase hex — node:crypto's `toString("hex")` encoding (password hashes,
/// `rsaKeyId`).
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Strict hex decode — any odd length or non-hex byte fails the whole thing.
/// (TS `Buffer.from(s, "hex")` silently truncates at bad input; callers here
/// treat malformed hex as "no match", which is the same outcome: the length
/// check downstream would fail anyway.)
pub(crate) fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        out.push(u8::from_str_radix(&s[i..i + 2], 16).ok()?);
    }
    Some(out)
}

#[cfg(test)]
pub(crate) fn test_rsa_pems() -> (String, String) {
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
    let mut rng = rand::rng();
    let private = rsa::PrivateKey::new(&mut rng, 2048).expect("rsa keygen");
    let private_pem = private
        .to_pkcs8_pem(LineEnding::LF)
        .expect("pkcs8 pem")
        .to_string();
    let public_pem = private
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .expect("spki pem");
    (private_pem, public_pem)
}

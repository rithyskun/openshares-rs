//! Hybrid RSA-OAEP(SHA-256) + AES-256-GCM credential envelope — port of
//! `src/lib/envelope.ts`.
//!
//! Credentials (login, password change) are encrypted client-side with the
//! deployment's RSA public key before they touch the wire — defense in depth
//! on top of TLS. Wire format is byte-compatible with the TS app:
//!
//! ```text
//! kid: key id — first 12 hex chars of sha256(public key PEM)
//! k:   base64(RSA-OAEP ciphertext of base64(32-byte AES-256 key))
//! i:   base64(12-byte AES-GCM IV)
//! d:   base64(AES-256-GCM ciphertext || 16-byte auth tag)
//! ```
//!
//! RSA key PEMs are stored single-line in env/secrets (`\n`-escaped, like the
//! TS `unescapePem`). RSA-OAEP decrypt is CPU-bound: prefer
//! [`decrypt_payload_async`] from async code.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::{Oaep, PrivateKey, PublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::crypto::{env, hex_encode, now_ms};
use crate::error::{AppError, Result};

/// `PAYLOAD_MAX_AGE_MS` — freshness window for the `ts` inside payloads.
pub const PAYLOAD_MAX_AGE_MS: i64 = 5 * 60 * 1000;

const AES_KEY_LEN: usize = 32;
const GCM_IV_LEN: usize = 12;

/// `HybridEnvelope` — the JSON envelope on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridEnvelope {
    pub kid: String,
    pub k: String,
    pub i: String,
    pub d: String,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn b64decode(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// RSA-OAEP with SHA-256 for both the label hash and MGF1 — the only
/// parameterization WebCrypto `RSA-OAEP` + `hash: "SHA-256"` produces.
fn oaep() -> Oaep<Sha256> {
    Oaep::new()
}

/// Client side of the envelope (`encryptPayload`) — the browser does this in
/// the TS app; here it exists for tests and for any first-party client that
/// wants to pre-encrypt credentials the same way.
pub fn encrypt_payload(plain_text: &str, public_key_pem: &str, key_id: &str) -> Result<String> {
    let public = PublicKey::from_public_key_pem(public_key_pem)
        .map_err(|e| AppError::Crypto(format!("bad RSA public key PEM: {e}")))?;

    let mut rng = rand::rng();
    let mut aes_key = [0u8; AES_KEY_LEN];
    let mut iv = [0u8; GCM_IV_LEN];
    rand::Rng::fill_bytes(&mut rng, &mut aes_key);
    rand::Rng::fill_bytes(&mut rng, &mut iv);

    let cipher = Aes256Gcm::new_from_slice(&aes_key)
        .map_err(|e| AppError::Crypto(format!("aes-gcm key init failed: {e}")))?;
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&iv), plain_text.as_bytes())
        .map_err(|e| AppError::Crypto(format!("aes-gcm encrypt failed: {e}")))?;

    // RSA-OAEP wraps the base64-encoded AES key (mirrors the reference: the
    // server base64-decodes after RSA decryption to recover the raw key).
    let enc_key = public
        .encrypt(&mut rng, oaep(), b64(&aes_key).as_bytes())
        .map_err(|e| AppError::Crypto(format!("rsa-oaep encrypt failed: {e}")))?;

    let envelope = HybridEnvelope {
        kid: key_id.to_string(),
        k: b64(&enc_key),
        i: b64(&iv),
        d: b64(&encrypted),
    };
    serde_json::to_string(&envelope)
        .map_err(|e| AppError::Crypto(format!("envelope encode failed: {e}")))
}

/// Server side (`decryptPayload`) — opens an envelope produced by
/// [`encrypt_payload`]. Returns `None` on any failure, never throws.
///
/// Unlike WebCrypto (which accepts any AES-GCM IV length), the Rust `aes-gcm`
/// API fixes the nonce at 12 bytes — anything else is rejected. Every
/// envelope the TS client produces uses a 12-byte IV, so this is compatible
/// in practice.
pub fn decrypt_payload(envelope_json: &str, private_key_pem: &str) -> Option<String> {
    let env0: HybridEnvelope = serde_json::from_str(envelope_json).ok()?;
    let private = PrivateKey::from_pkcs8_pem(private_key_pem).ok()?;
    let aes_key_b64 = private
        .decrypt_blinded(&mut rand::rng(), oaep(), &b64decode(&env0.k)?)
        .ok()?;
    let aes_key = b64decode(std::str::from_utf8(&aes_key_b64).ok()?)?;
    let cipher = Aes256Gcm::new_from_slice(&aes_key).ok()?;
    let iv = b64decode(&env0.i)?;
    if iv.len() != GCM_IV_LEN {
        return None;
    }
    let plain = cipher
        .decrypt(Nonce::from_slice(&iv), b64decode(&env0.d)?.as_slice())
        .ok()?;
    String::from_utf8(plain).ok()
}

/// `spawn_blocking` wrapper — RSA-OAEP decrypt must not stall the runtime.
pub async fn decrypt_payload_async(
    envelope_json: String,
    private_key_pem: String,
) -> Result<Option<String>> {
    tokio::task::spawn_blocking(move || decrypt_payload(&envelope_json, &private_key_pem))
        .await
        .map_err(|e| AppError::Crypto(format!("rsa worker failed: {e}")))
}

/* ------------------------------ server config ----------------------------- */

/// `unescapePem` — PEMs are stored single-line in env/secrets.
/// `pub(crate)` — `jwt.rs::from_env` applies it to the JWT keys too
/// (docker `--env-file` and other env transports deliver `\n` escapes).
pub(crate) fn unescape_pem(pem: &str) -> String {
    pem.replace("\\n", "\n")
}

/// `rsaEnabled` — encryption is mandatory when both keys are configured.
pub fn rsa_enabled() -> bool {
    env("RSA_PRIVATE_KEY").is_some() && env("RSA_PUBLIC_KEY").is_some()
}

/// `rsaPublicKeyPem` — served to clients via `GET /api/auth/rsa-key`.
pub fn rsa_public_key_pem() -> Option<String> {
    env("RSA_PUBLIC_KEY").map(|p| unescape_pem(&p))
}

/// `rsaPrivateKeyPem`.
pub fn rsa_private_key_pem() -> Option<String> {
    env("RSA_PRIVATE_KEY").map(|p| unescape_pem(&p))
}

/// `rsaKeyId` — first 12 hex chars of sha256(public key PEM); lets clients
/// pin which key they encrypted with.
pub fn rsa_key_id(public_key_pem: &str) -> String {
    let digest = Sha256::digest(public_key_pem.as_bytes());
    hex_encode(&digest)[..12].to_string()
}

/// `payloadFresh` — freshness check on the `ts` field inside encrypted
/// credential payloads (unix ms, ±5 min).
pub fn payload_fresh(ts: f64) -> bool {
    ts.is_finite() && (now_ms() as f64 - ts).abs() <= PAYLOAD_MAX_AGE_MS as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip() {
        let (private_pem, public_pem) = crate::crypto::test_rsa_pems();
        let kid = rsa_key_id(&public_pem);
        assert_eq!(kid.len(), 12);
        assert!(kid.bytes().all(|b| b.is_ascii_hexdigit()));

        let inner = r#"{"email":"u@x.co","password":"pw","ts":1700000000000}"#;
        let wire = encrypt_payload(inner, &public_pem, &kid).unwrap();

        // wire shape: {kid,k,i,d} all strings
        let env0: HybridEnvelope = serde_json::from_str(&wire).unwrap();
        assert_eq!(env0.kid, kid);
        assert!(env0.k.len() > 100); // rsa-2048 ciphertext b64
        assert_eq!(b64decode(&env0.i).unwrap().len(), 12);

        assert_eq!(decrypt_payload(&wire, &private_pem).as_deref(), Some(inner));
    }

    #[test]
    fn decrypt_failures_return_none() {
        let (private_pem, public_pem) = crate::crypto::test_rsa_pems();
        let wire = encrypt_payload("x", &public_pem, "kid").unwrap();

        // wrong private key
        let (other_priv, _) = crate::crypto::test_rsa_pems();
        assert!(decrypt_payload(&wire, &other_priv).is_none());

        // garbage json / bad fields
        assert!(decrypt_payload("not json", &private_pem).is_none());
        assert!(decrypt_payload(r#"{"kid":1,"k":2}"#, &private_pem).is_none());

        // tampered ciphertext → gcm tag check fails
        let mut env0: HybridEnvelope = serde_json::from_str(&wire).unwrap();
        env0.d = b64(b"tampered");
        let tampered = serde_json::to_string(&env0).unwrap();
        assert!(decrypt_payload(&tampered, &private_pem).is_none());
    }

    #[tokio::test]
    async fn async_decrypt_roundtrip() {
        let (private_pem, public_pem) = crate::crypto::test_rsa_pems();
        let wire = encrypt_payload("payload", &public_pem, "kid").unwrap();
        assert_eq!(
            decrypt_payload_async(wire, private_pem)
                .await
                .unwrap()
                .as_deref(),
            Some("payload")
        );
    }

    #[test]
    fn payload_freshness() {
        // Keep a 60s margin from the boundary — a real now_ms() call between
        // computing `now` and invoking payload_fresh makes exact-edge asserts
        // racy.
        let now = now_ms() as f64;
        let margin = 60_000.0;
        assert!(payload_fresh(now));
        assert!(payload_fresh(now - PAYLOAD_MAX_AGE_MS as f64 + margin));
        assert!(payload_fresh(now + PAYLOAD_MAX_AGE_MS as f64 - margin));
        assert!(!payload_fresh(now - PAYLOAD_MAX_AGE_MS as f64 - margin));
        assert!(!payload_fresh(now + PAYLOAD_MAX_AGE_MS as f64 + margin));
        assert!(!payload_fresh(f64::NAN));
        assert!(!payload_fresh(f64::INFINITY));
    }

    #[test]
    fn pem_unescaping() {
        assert_eq!(
            unescape_pem("-----BEGIN X-----\\nabc\\n-----END X-----"),
            "-----BEGIN X-----\nabc\n-----END X-----"
        );
        // round-trip: an escaped PEM decrypts the same as the real one
        let (private_pem, public_pem) = crate::crypto::test_rsa_pems();
        let escaped = private_pem.replace('\n', "\\n");
        assert_eq!(unescape_pem(&escaped), private_pem);
        let wire = encrypt_payload("ok", &public_pem, "k").unwrap();
        assert_eq!(
            decrypt_payload(&wire, &unescape_pem(&escaped)).as_deref(),
            Some("ok")
        );
    }
}

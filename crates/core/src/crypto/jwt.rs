//! RS256 session JWTs — port of `src/lib/jwt.ts`.
//!
//! The session cookie carries `{ sub, sid }` (`sub` = user id as a string);
//! the `sessions` row keyed by `sid` stays authoritative for revocation, so
//! logout/suspension kill sessions immediately. Sign with `JWT_PRIVATE_KEY`
//! (pkcs8 PEM), verify with `JWT_PUBLIC_KEY` (spki PEM) — same env vars and
//! key formats as the TS deployment.
//!
//! Unlike `jwt.ts` (which lazy-loads each key on first use and throws when
//! unset), keys are parsed once up front — [`SessionJwt::from_env`] returns a
//! `Config` error when either var is missing, matching the TS error text.

use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::db::now;
use crate::error::{AppError, Result};

/// `SessionClaims` — `{ sub: userId, sid: sessionRowId }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionClaims {
    pub sub: String,
    pub sid: String,
}

#[derive(Serialize)]
struct SignClaims<'a> {
    sub: &'a str,
    sid: &'a str,
    iat: u64,
    exp: u64,
}

#[derive(Deserialize)]
struct JwtPayload {
    sub: String,
    sid: String,
}

/// Parsed JWT keypair — the `privateKey()`/`publicKey()` pair from `jwt.ts`,
/// eagerly imported at boot instead of lazily per call.
pub struct SessionJwt {
    enc: EncodingKey,
    dec: DecodingKey,
}

impl SessionJwt {
    /// Load `JWT_PRIVATE_KEY` + `JWT_PUBLIC_KEY` from the environment.
    /// `unescape_pem` normalizes single-line `\n`-escaped PEMs — needed
    /// when the env transport can't carry real newlines (`docker
    /// --env-file`, most platforms' env vars; Workers secrets could).
    pub fn from_env() -> Result<Self> {
        let private_pem = crate::crypto::env("JWT_PRIVATE_KEY").ok_or_else(|| {
            AppError::Config("JWT_PRIVATE_KEY is not set — run `npm run gen-jwt-keys`.".to_string())
        })?;
        let public_pem = crate::crypto::env("JWT_PUBLIC_KEY").ok_or_else(|| {
            AppError::Config("JWT_PUBLIC_KEY is not set — run `npm run gen-jwt-keys`.".to_string())
        })?;
        Self::from_pems(
            &crate::crypto::envelope::unescape_pem(&private_pem),
            &crate::crypto::envelope::unescape_pem(&public_pem),
        )
    }

    /// Import a pkcs8 private PEM + spki public PEM.
    pub fn from_pems(private_pem: &str, public_pem: &str) -> Result<Self> {
        // jsonwebtoken 10 requires an installed CryptoProvider before any
        // encode/decode. `rust_crypto` is pure Rust (aws_lc_rs needs a C
        // toolchain on Windows). Install is process-global and at most
        // once — a repeat call returns Err, harmless to ignore.
        let _ = jsonwebtoken::crypto::rust_crypto::DEFAULT_PROVIDER.install_default();
        let enc = EncodingKey::from_rsa_pem(private_pem.as_bytes())
            .map_err(|e| AppError::Crypto(format!("bad JWT_PRIVATE_KEY PEM: {e}")))?;
        let dec = DecodingKey::from_rsa_pem(public_pem.as_bytes())
            .map_err(|e| AppError::Crypto(format!("bad JWT_PUBLIC_KEY PEM: {e}")))?;
        Ok(Self { enc, dec })
    }

    /// `signSessionJwt` — `{ sub, sid, iat: now, exp: now + ttlSec }`.
    pub fn sign(&self, user_id: i64, sid: &str, ttl_secs: i64) -> Result<String> {
        let issued_at = now().max(0) as u64;
        let sub = user_id.to_string();
        let claims = SignClaims {
            sub: &sub,
            sid,
            iat: issued_at,
            // negative ttl → exp in the past, i.e. an already-dead token
            exp: (issued_at as i64).saturating_add(ttl_secs).max(0) as u64,
        };
        jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &self.enc)
            .map_err(|e| AppError::Crypto(format!("jwt sign failed: {e}")))
    }

    /// `verifySessionJwt` — signature + `exp` check. Returns claims or
    /// `None`; never fails (mirrors the TS try/catch → null).
    pub fn verify(&self, token: &str) -> Option<SessionClaims> {
        let mut validation = Validation::new(Algorithm::RS256);
        validation.leeway = 0; // jose uses no leeway
        validation.validate_aud = false; // jose doesn't check aud
        validation.validate_nbf = true; // jose validates nbf when present
        let payload = jsonwebtoken::decode::<JwtPayload>(token, &self.dec, &validation)
            .ok()?
            .claims;
        Some(SessionClaims {
            sub: payload.sub,
            sid: payload.sid,
        })
    }
}

/// `peekSessionJwt` — claims WITHOUT verification, for best-effort cleanup
/// (logout of expired JWTs). `None` unless both `sub` and `sid` are strings.
pub fn peek_session_jwt(token: &str) -> Option<SessionClaims> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let sub = value.get("sub")?.as_str()?;
    let sid = value.get("sid")?.as_str()?;
    Some(SessionClaims {
        sub: sub.to_string(),
        sid: sid.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_roundtrip() {
        let (private_pem, public_pem) = crate::crypto::test_rsa_pems();
        let jwt = SessionJwt::from_pems(&private_pem, &public_pem).unwrap();
        let token = jwt.sign(42, "sid-abc", 3600).unwrap();
        let claims = jwt.verify(&token).unwrap();
        assert_eq!(
            claims,
            SessionClaims {
                sub: "42".into(),
                sid: "sid-abc".into()
            }
        );
    }

    #[test]
    fn peek_reads_claims_without_key() {
        let (private_pem, public_pem) = crate::crypto::test_rsa_pems();
        let jwt = SessionJwt::from_pems(&private_pem, &public_pem).unwrap();
        let token = jwt.sign(7, "sid-xyz", 3600).unwrap();
        assert_eq!(
            peek_session_jwt(&token).unwrap(),
            SessionClaims {
                sub: "7".into(),
                sid: "sid-xyz".into()
            }
        );
        assert!(peek_session_jwt("not-a-jwt").is_none());
        assert!(peek_session_jwt("a.b.c").is_none());
    }

    #[test]
    fn expired_token_rejected() {
        let (private_pem, public_pem) = crate::crypto::test_rsa_pems();
        let jwt = SessionJwt::from_pems(&private_pem, &public_pem).unwrap();
        let token = jwt.sign(1, "sid", -10).unwrap();
        assert!(jwt.verify(&token).is_none());
        // peek still returns claims — that's its job.
        assert!(peek_session_jwt(&token).is_some());
    }

    #[test]
    fn tampered_and_wrong_key_rejected() {
        let (private_pem, public_pem) = crate::crypto::test_rsa_pems();
        let jwt = SessionJwt::from_pems(&private_pem, &public_pem).unwrap();
        let token = jwt.sign(1, "sid", 3600).unwrap();
        let mut tampered = token;
        tampered.push('x');
        assert!(jwt.verify(&tampered).is_none());

        // a verifier built from a different keypair must reject it
        let (other_priv, other_pub) = crate::crypto::test_rsa_pems();
        let other = SessionJwt::from_pems(&other_priv, &other_pub).unwrap();
        let token2 = other.sign(1, "sid", 3600).unwrap();
        assert!(jwt.verify(&token2).is_none());
    }

    #[test]
    fn malformed_time_claims_rejected() {
        // jsonwebtoken <10.3.0 (CVE-2026-25537) silently skipped exp/nbf
        // validation when the claims weren't numbers — a signed token with
        // string `exp`/`nbf` would verify as never-expiring. 10.3+ fails
        // closed: unparseable time claims reject the token.
        let (private_pem, public_pem) = crate::crypto::test_rsa_pems();
        let jwt = SessionJwt::from_pems(&private_pem, &public_pem).unwrap();

        #[derive(Serialize)]
        struct BadClaims<'a> {
            sub: &'a str,
            sid: &'a str,
            exp: &'a str,
            nbf: &'a str,
        }
        let claims = BadClaims {
            sub: "1",
            sid: "sid",
            exp: "not-a-number",
            nbf: "also-bad",
        };
        let token =
            jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &jwt.enc).unwrap();
        assert!(jwt.verify(&token).is_none());
    }
}

//! Password hashing + session persistence — port of `src/lib/auth.ts`.
//!
//! Covers the non-Next.js half of `auth.ts`: `hashPassword`,
//! `verifyPassword`, and the `sessions`/`users` table queries behind
//! `createSession`, `destroySession`, `getSessionUserByToken` and
//! `cleanupExpiredSessions`. Cookie plumbing lives in the server crate;
//! JWT signing/verifying lives in [`crate::crypto::jwt`].
//!
//! Hash format is byte-compatible with node:crypto:
//! `<salt_hex>:<hash_hex>` where the salt is 16 random bytes hex-encoded
//! and the hash is `scryptSync(password, salt, 64)` — node defaults
//! `N=2^14, r=8, p=1` — using the hex **string** bytes as the scrypt salt
//! (node passes `salt` through verbatim as a string, not decoded bytes).
//!
//! scrypt is CPU-bound (~50 ms at these params): the `*_async` wrappers
//! offload to `tokio::task::spawn_blocking`.

use subtle::ConstantTimeEq;

use crate::crypto::{hex_decode, hex_encode};
use crate::db::{Db, now};
use crate::error::{AppError, Result};

const SCRYPT_LOG_N: u8 = 14; // node default N = 16384
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;
const KEY_LEN: usize = 64;
const SALT_LEN: usize = 16;

/// `SESSION_TTL` — session row lifetime, unix seconds (1 day).
pub const SESSION_TTL_SECS: i64 = 24 * 3600;

fn scrypt_params() -> Result<scrypt::Params> {
    scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, KEY_LEN)
        .map_err(|e| AppError::Crypto(format!("invalid scrypt params: {e}")))
}

fn scrypt64(password: &str, salt: &str) -> Result<[u8; KEY_LEN]> {
    let mut out = [0u8; KEY_LEN];
    scrypt::scrypt(
        password.as_bytes(),
        salt.as_bytes(),
        &scrypt_params()?,
        &mut out,
    )
    .map_err(|e| AppError::Crypto(format!("scrypt failed: {e}")))?;
    Ok(out)
}

/// `hashPassword` — `"<salt_hex>:<hash_hex>"`.
pub fn hash_password(password: &str) -> Result<String> {
    let mut salt = [0u8; SALT_LEN];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut salt);
    let salt_hex = hex_encode(&salt);
    let hash = scrypt64(password, &salt_hex)?;
    Ok(format!("{salt_hex}:{}", hex_encode(&hash)))
}

/// `verifyPassword` — constant-time compare, false on any malformed input.
pub fn verify_password(password: &str, stored: Option<&str>) -> bool {
    let Some(stored) = stored else { return false };
    let mut parts = stored.split(':');
    let (Some(salt), Some(hash_hex)) = (parts.next(), parts.next()) else {
        return false;
    };
    if salt.is_empty() || hash_hex.is_empty() {
        return false;
    }
    let Some(expected) = hex_decode(hash_hex) else {
        return false;
    };
    let Ok(candidate) = scrypt64(password, salt) else {
        return false;
    };
    // candidate.len() === expected.len() && timingSafeEqual — ct_eq covers both.
    candidate[..].ct_eq(&expected[..]).into()
}

/// `spawn_blocking` wrapper — the login route should call this, not the sync
/// version, so the runtime never stalls on scrypt.
pub async fn hash_password_async(password: String) -> Result<String> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| AppError::Crypto(format!("scrypt worker failed: {e}")))?
}

/// `spawn_blocking` wrapper around [`verify_password`].
pub async fn verify_password_async(password: String, stored: Option<String>) -> Result<bool> {
    tokio::task::spawn_blocking(move || verify_password(&password, stored.as_deref()))
        .await
        .map_err(|e| AppError::Crypto(format!("scrypt worker failed: {e}")))
}

/* ------------------------------ sessions ------------------------------ */

/// `SessionUser` — the joined users+sessions row a valid session resolves to.
#[derive(Debug, Clone)]
pub struct SessionUser {
    pub id: i64,
    pub email: Option<String>,
    pub name: String,
    pub telegram_id: Option<String>,
    /// session row id — also the request-signing key derivation input.
    pub sid: String,
    /// when this session was created (the "signed in at" time).
    pub session_created_at: Option<i64>,
    /// The user's resolved permission keys — fetched alongside the session
    /// so `can()` checks don't each cost a remote round trip.
    pub permissions: std::sync::Arc<std::collections::HashSet<String>>,
}

impl SessionUser {
    /// `can(userId, key, ownerId)` — in-memory check over the perms fetched
    /// with the session (same rule as `rbac::can`: `*` | `key` | `key:any`,
    /// or `key:own` when `owner_id` is the caller).
    pub fn can(&self, key: &str, owner_id: Option<i64>) -> bool {
        let p = &self.permissions;
        p.contains("*")
            || p.contains(key)
            || p.contains(&format!("{key}:any"))
            || (owner_id == Some(self.id) && p.contains(&format!("{key}:own")))
    }
}

/// Joined users+sessions row backing [`get_session_user_by_token`].
#[derive(serde::Deserialize)]
struct SessionRow {
    id: i64,
    email: Option<String>,
    name: String,
    telegram_id: Option<String>,
    session_created_at: Option<i64>,
}

/// `createSession` — inserts the revocation anchor row and returns the new
/// `sid`. Signing the JWT is the caller's job (`SessionJwt::sign`), which
/// mirrors the TS flow (cookie write is also left to the server).
pub async fn create_session(db: &Db, user_id: i64) -> Result<String> {
    let sid = crate::util::new_token(24);
    db.execute(
        "INSERT INTO sessions (id, user_id, expires_at, created_at) VALUES (?, ?, ?, ?)",
        libsql::params![sid.as_str(), user_id, now() + SESSION_TTL_SECS, now()],
    )
    .await?;
    Ok(sid)
}

/// `DELETE FROM sessions WHERE id = ?` — revocation primitive.
pub async fn delete_session(db: &Db, sid: &str) -> Result<()> {
    db.execute("DELETE FROM sessions WHERE id = ?", libsql::params![sid])
        .await?;
    Ok(())
}

/// `destroySession` — peeks the (possibly expired) JWT and deletes the
/// session row it names. No row → no-op, like the TS version.
pub async fn destroy_session_by_token(db: &Db, token: &str) -> Result<()> {
    if let Some(claims) = crate::crypto::jwt::peek_session_jwt(token) {
        delete_session(db, &claims.sid).await?;
    }
    Ok(())
}

/// `getSessionUserByToken` — JWT signature + `exp`, then the session row
/// (`sid` + `user_id` + `expires_at > now`) and `u.disabled_at IS NULL`.
/// A valid signature on a deleted/suspended session is still rejected.
pub async fn get_session_user_by_token(
    db: &Db,
    jwt: &crate::crypto::jwt::SessionJwt,
    token: &str,
) -> Result<Option<SessionUser>> {
    let Some(claims) = jwt.verify(token) else {
        return Ok(None);
    };
    // claims.sub is the user id stringified; a non-numeric sub can't match.
    let Ok(user_id) = claims.sub.parse::<i64>() else {
        return Ok(None);
    };
    // Session row and permissions are independent lookups — run them
    // concurrently so they cost one remote round trip, not two. (A dead
    // session wastes the perms query, but it's parallel either way.)
    let (row, perms) = tokio::join!(
        db.query_opt::<SessionRow>(
            "SELECT u.id, u.email, u.name, u.telegram_id, s.created_at AS session_created_at
           FROM sessions s JOIN users u ON u.id = s.user_id
          WHERE s.id = ? AND s.user_id = ? AND s.expires_at > ? AND u.disabled_at IS NULL",
            libsql::params![claims.sid.as_str(), user_id, now()],
        ),
        crate::rbac::permissions_of(db, user_id),
    );
    let row = row?;
    let perms = perms?;
    Ok(row.map(|r| SessionUser {
        id: r.id,
        email: r.email,
        name: r.name,
        telegram_id: r.telegram_id,
        sid: claims.sid,
        session_created_at: r.session_created_at,
        permissions: std::sync::Arc::new(perms),
    }))
}

/// `cleanupExpiredSessions` — periodic sweeper.
pub async fn cleanup_expired_sessions(db: &Db) -> Result<()> {
    db.execute(
        "DELETE FROM sessions WHERE expires_at <= ?",
        libsql::params![now()],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_format_and_verify() {
        let stored = hash_password("s3cret").unwrap();
        let (salt, hash) = stored.split_once(':').unwrap();
        assert_eq!(salt.len(), 32);
        assert!(salt.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(hash.len(), 128);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));

        assert!(verify_password("s3cret", Some(&stored)));
        assert!(!verify_password("wrong", Some(&stored)));
    }

    #[test]
    fn verify_rejects_malformed() {
        assert!(!verify_password("x", None));
        assert!(!verify_password("x", Some("")));
        assert!(!verify_password("x", Some("salt")));
        assert!(!verify_password("x", Some(":")));
        assert!(!verify_password("x", Some(":aa")));
        assert!(!verify_password("x", Some("aa:")));
        // non-hex hash → same "no match" the TS length check would produce
        assert!(!verify_password("x", Some("aabb:zz")));
    }

    #[tokio::test]
    async fn async_wrappers_roundtrip() {
        let stored = hash_password_async("pw".to_string()).await.unwrap();
        assert!(
            verify_password_async("pw".to_string(), Some(stored.clone()))
                .await
                .unwrap()
        );
        assert!(
            !verify_password_async("nope".to_string(), Some(stored))
                .await
                .unwrap()
        );
    }

    /* ------------------------- session queries ------------------------- */

    async fn test_db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("test.db").display());
        let db = Db::connect(&url).await.unwrap();
        db.migrate().await.unwrap();
        (db, dir)
    }

    async fn insert_user(db: &Db) -> i64 {
        db.execute(
            "INSERT INTO users (email, password_hash, name, created_at, updated_at)
             VALUES ('u@x.co', 'h', 'User', 1, 1)",
            (),
        )
        .await
        .unwrap();
        db.conn()
            .query("SELECT id FROM users WHERE email = 'u@x.co'", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap()
    }

    fn test_jwt() -> crate::crypto::jwt::SessionJwt {
        let (priv_pem, pub_pem) = crate::crypto::test_rsa_pems();
        crate::crypto::jwt::SessionJwt::from_pems(&priv_pem, &pub_pem).unwrap()
    }

    #[tokio::test]
    async fn session_lifecycle() {
        let (db, _dir) = test_db().await;
        let uid = insert_user(&db).await;
        let jwt = test_jwt();

        let sid = create_session(&db, uid).await.unwrap();
        let token = jwt.sign(uid, &sid, SESSION_TTL_SECS).unwrap();
        let user = get_session_user_by_token(&db, &jwt, &token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(user.id, uid);
        assert_eq!(user.email.as_deref(), Some("u@x.co"));
        assert_eq!(user.sid, sid);
        assert!(user.session_created_at.is_some());

        destroy_session_by_token(&db, &token).await.unwrap();
        assert!(
            get_session_user_by_token(&db, &jwt, &token)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn disabled_and_expired_sessions_rejected() {
        let (db, _dir) = test_db().await;
        let uid = insert_user(&db).await;
        let jwt = test_jwt();

        // disabled user → None even with a valid signature + live session
        let sid = create_session(&db, uid).await.unwrap();
        let token = jwt.sign(uid, &sid, SESSION_TTL_SECS).unwrap();
        db.execute(
            "UPDATE users SET disabled_at = ? WHERE id = ?",
            libsql::params![now(), uid],
        )
        .await
        .unwrap();
        assert!(
            get_session_user_by_token(&db, &jwt, &token)
                .await
                .unwrap()
                .is_none()
        );
        db.execute(
            "UPDATE users SET disabled_at = NULL WHERE id = ?",
            libsql::params![uid],
        )
        .await
        .unwrap();

        // expired session row → None (signature is still valid)
        db.execute(
            "UPDATE sessions SET expires_at = ? WHERE id = ?",
            libsql::params![now() - 1, sid.as_str()],
        )
        .await
        .unwrap();
        assert!(
            get_session_user_by_token(&db, &jwt, &token)
                .await
                .unwrap()
                .is_none()
        );

        // garbage token → None
        assert!(
            get_session_user_by_token(&db, &jwt, "garbage")
                .await
                .unwrap()
                .is_none()
        );

        cleanup_expired_sessions(&db).await.unwrap();
        let n: i64 = db
            .conn()
            .query("SELECT COUNT(*) FROM sessions", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(n, 0);
    }
}

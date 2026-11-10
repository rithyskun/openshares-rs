//! Rate limiting + request guards — port of `src/lib/security.ts`.
//!
//! Deterministic fixed-window rate limiting backed by the `rate_limits`
//! table (single-statement upsert — atomic under SQLite so concurrent hits
//! can't lose counts). **Fails open when the DB is unreachable** — rate
//! limiting is defense-in-depth, not an auth check.
//!
//! The [`Limiter`] names are the `LIMITS` keys from `security.ts` and double
//! as the `rate_limits.bucket` prefix, so buckets stay compatible with rows
//! written by the TS app.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::db::{Db, now};
use crate::error::{AppError, Result};

/// `LimiterName` — the `LIMITS` table from `security.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limiter {
    /// anonymous auth endpoints (login, code minting) — keyed by client IP
    Auth,
    /// authenticated write endpoints — keyed by user id
    Api,
    /// middleware phase-1 (IP-keyed): brute force on /api/auth/*
    MwAuth,
    /// middleware phase-1: Telegram webhook bursts
    MwWebhook,
    /// middleware phase-1: reads — galleries burst thumbnail fetches
    MwRead,
    /// middleware phase-1: writes
    MwWrite,
}

impl Limiter {
    /// TS `LIMITS` key — the `rate_limits.bucket` prefix (`"{name}:{key}"`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Auth => "AUTH_LIMITER",
            Self::Api => "API_LIMITER",
            Self::MwAuth => "MW_AUTH",
            Self::MwWebhook => "MW_WEBHOOK",
            Self::MwRead => "MW_READ",
            Self::MwWrite => "MW_WRITE",
        }
    }

    /// Requests allowed per window (a hit is rejected when `count > limit`).
    pub fn limit(self) -> i64 {
        match self {
            Self::Auth => 10,
            Self::Api => 120,
            Self::MwAuth => 10,
            Self::MwWebhook => 240,
            Self::MwRead => 600,
            Self::MwWrite => 120,
        }
    }

    /// Window length in seconds.
    pub fn window_secs(self) -> i64 {
        60
    }
}

// Single-statement upsert: resets the window when expired, else increments.
// Same SQL as security.ts (positional params ?1/?2/?3).
const COUNT_SQL: &str = "
  INSERT INTO rate_limits (bucket, window_start, count)
  VALUES (?1, ?2, 1)
  ON CONFLICT(bucket) DO UPDATE SET
    count        = CASE WHEN rate_limits.window_start + ?3 <= ?2 THEN 1 ELSE rate_limits.count + 1 END,
    window_start = CASE WHEN rate_limits.window_start + ?3 <= ?2 THEN ?2 ELSE rate_limits.window_start END
  RETURNING count";

/// `RateLimitInfo` — counter state for `X-RateLimit-*` headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitInfo {
    pub count: i64,
    pub limit: i64,
    /// unix seconds when the current window resets
    pub reset: i64,
}

/// `rateLimitInfo` — like [`rate_limited`] but returns the counter state so
/// callers can emit `X-RateLimit-*` headers. Fails open: `count = 0` on DB
/// errors.
pub async fn rate_limit_info(db: &Db, name: Limiter, key: &str) -> RateLimitInfo {
    let t = now();
    let info = RateLimitInfo {
        count: 0,
        limit: name.limit(),
        reset: t + name.window_secs(),
    };
    match rate_limit_count(db, name, key, t).await {
        Ok(count) => RateLimitInfo { count, ..info },
        Err(e) => {
            tracing::warn!(error = %e, limiter = name.name(), "rate limiter failed open");
            info
        }
    }
}

async fn rate_limit_count(db: &Db, name: Limiter, key: &str, t: i64) -> Result<i64> {
    // Read inside `next()` — local-driver rows are bound to the shared
    // statement cursor (see `Db::conn` caveat).
    let mut rows = db
        .query_guard(
            COUNT_SQL,
            libsql::params![format!("{}:{key}", name.name()), t, name.window_secs()],
        )
        .await?;
    let count: i64 = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => return Err(AppError::Database(libsql::Error::QueryReturnedNoRows)),
    };
    // ~2% of requests sweep stale buckets, like the TS version.
    if rand::RngExt::random::<f64>(&mut rand::rng()) < 0.02 {
        db.execute(
            "DELETE FROM rate_limits WHERE window_start < ?",
            libsql::params![t - 3600],
        )
        .await?;
    }
    Ok(count)
}

/// `rateLimited` — `true` once the bucket overflows (`count > limit`).
pub async fn rate_limited(db: &Db, name: Limiter, key: &str) -> bool {
    let info = rate_limit_info(db, name, key).await;
    info.count > info.limit
}

/// `clientIp` — cf-connecting-ip wins, else the first XFF hop, else
/// "unknown".
pub fn client_ip(cf_connecting_ip: Option<&str>, x_forwarded_for: Option<&str>) -> String {
    if let Some(ip) = cf_connecting_ip {
        return ip.to_string();
    }
    if let Some(first) = x_forwarded_for.and_then(|v| v.split(',').next()) {
        return first.trim().to_string();
    }
    "unknown".to_string()
}

/// `tooManyRequests` — 429. (The TS version emits `Retry-After: 60` on the
/// response; header plumbing is the server crate's concern.)
pub fn too_many_requests() -> AppError {
    AppError::RateLimited
}

/// `secretEq` — constant-time secret comparison. SHA-256s both sides first
/// (the TS hash-fallback path) so the compare never early-exits on a
/// length/prefix difference.
pub fn secret_eq(given: &str, expected: &str) -> bool {
    let a = Sha256::digest(given.as_bytes());
    let b = Sha256::digest(expected.as_bytes());
    a[..].ct_eq(&b[..]).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_equality() {
        assert!(secret_eq("abc", "abc"));
        assert!(secret_eq("", ""));
        assert!(!secret_eq("abc", "abd"));
        assert!(!secret_eq("abc", "abcd"));
        assert!(!secret_eq("", "x"));
    }

    #[test]
    fn client_ip_parsing() {
        assert_eq!(client_ip(Some("1.2.3.4"), Some("9.9.9.9")), "1.2.3.4");
        assert_eq!(client_ip(None, Some("5.6.7.8, 9.9.9.9")), "5.6.7.8");
        assert_eq!(client_ip(None, Some("  5.6.7.8 ,9.9.9.9")), "5.6.7.8");
        assert_eq!(client_ip(None, None), "unknown");
        // empty XFF still yields "" — same as the TS `??` (not nullish)
        assert_eq!(client_ip(None, Some("")), "");
    }

    async fn test_db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("test.db").display());
        let db = Db::connect(&url).await.unwrap();
        db.migrate().await.unwrap();
        (db, dir)
    }

    #[tokio::test]
    async fn rate_limit_counts_and_limits() {
        let (db, _dir) = test_db().await;

        let info = rate_limit_info(&db, Limiter::MwAuth, "ip-1").await;
        assert_eq!((info.count, info.limit), (1, 10));
        assert!(info.reset > now());
        assert!(!rate_limited(&db, Limiter::MwAuth, "ip-1").await);

        // bucket key is "{LIMITER_NAME}:{key}" — compatible with TS rows
        let bucket: String = db
            .query_guard("SELECT bucket FROM rate_limits", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(bucket, "MW_AUTH:ip-1");

        // burn through the limit: each call increments first (count is now
        // 2), so counts 3..=10 stay under the wire and the 11th hit
        // overflows (count > limit).
        for _ in 3..=10 {
            assert!(!rate_limited(&db, Limiter::MwAuth, "ip-1").await);
        }
        assert!(rate_limited(&db, Limiter::MwAuth, "ip-1").await); // count == 11

        // a different key has its own bucket
        assert!(!rate_limited(&db, Limiter::MwAuth, "ip-2").await);
    }

    #[tokio::test]
    async fn expired_window_resets() {
        let (db, _dir) = test_db().await;
        db.execute(
            "INSERT INTO rate_limits (bucket, window_start, count) VALUES (?, ?, ?)",
            libsql::params!["MW_READ:ip-9", now() - 3600, 999i64],
        )
        .await
        .unwrap();

        let info = rate_limit_info(&db, Limiter::MwRead, "ip-9").await;
        assert_eq!(info.count, 1); // window expired → counter reset
    }

    #[tokio::test]
    async fn db_failure_fails_open() {
        let (db, _dir) = test_db().await;
        // libsql has no pool `close()` — drop the table so every query
        // errors, exercising the same fail-open path.
        db.execute_batch("DROP TABLE rate_limits").await.unwrap();

        let info = rate_limit_info(&db, Limiter::Auth, "ip-x").await;
        assert_eq!(info.count, 0);
        assert_eq!(info.limit, 10);
        assert!(!rate_limited(&db, Limiter::Auth, "ip-x").await);
    }

    #[test]
    fn too_many_requests_maps_to_rate_limited() {
        assert!(matches!(too_many_requests(), AppError::RateLimited));
    }
}

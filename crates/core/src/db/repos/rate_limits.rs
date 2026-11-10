//! Rate-limits repository — ports the `rate_limits` fixed-window
//! counter in `src/lib/security.ts` (`rateLimitInfo`/`rateLimited`).
//!
//! Raw table helpers only: the limiter names, limits, windows, and the
//! fail-open policy live in `crate::crypto::security` (same split as
//! security.ts owning `LIMITS` over this table). `bucket` arrives
//! already namespaced (`"<LIMITER>:<key>"` — e.g. `"AUTH_LIMITER:1.2.3.4"`).

use crate::db::Db;
use crate::error::Result;
use crate::models::RateLimit;

/// `COUNT_SQL` verbatim — the single-statement upsert that resets the
/// window when it expired, else increments. Atomic under SQLite so
/// concurrent hits can't lose counts; `RETURNING count` is the
/// post-increment value the limiter compares against its limit.
const COUNT_SQL: &str = "
  INSERT INTO rate_limits (bucket, window_start, count)
  VALUES (?1, ?2, 1)
  ON CONFLICT(bucket) DO UPDATE SET
    count        = CASE WHEN rate_limits.window_start + ?3 <= ?2 THEN 1 ELSE rate_limits.count + 1 END,
    window_start = CASE WHEN rate_limits.window_start + ?3 <= ?2 THEN ?2 ELSE rate_limits.window_start END
  RETURNING count";

/// One hit on `bucket` for the window starting at `at` — returns the
/// window's post-increment count (1 for a fresh window). `window_secs`
/// is the limiter's window size, not computed here.
pub async fn hit(db: &Db, bucket: &str, at: i64, window_secs: i64) -> Result<i64> {
    db.scalar::<i64>(COUNT_SQL, libsql::params![bucket, at, window_secs])
        .await
}

/// Read the current row — diagnostics/tests; the hot path only uses
/// [`hit`].
pub async fn get(db: &Db, bucket: &str) -> Result<Option<RateLimit>> {
    db.query_opt::<RateLimit>(
        "SELECT * FROM rate_limits WHERE bucket = ?",
        libsql::params![bucket],
    )
    .await
}

/// `DELETE FROM rate_limits WHERE window_start < ?` — the opportunistic
/// sweep security.ts runs on ~2% of hits (`t - 3600`). Returns rows
/// removed.
pub async fn sweep(db: &Db, window_started_before: i64) -> Result<u64> {
    db.execute(
        "DELETE FROM rate_limits WHERE window_start < ?",
        libsql::params![window_started_before],
    )
    .await
}

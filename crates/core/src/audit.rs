//! Append-only audit trail — port of `src/lib/audit.ts`.
//!
//! Events are JSONL lines in monthly shards `audit/YYYY-MM.jsonl` inside
//! the media cache (R2/`S3Cache` in prod; `FsCache` locally — same key
//! space, so
//! a future object-store backend slots in without touching callers).
//! `audit()` is best-effort: it logs and swallows failures, matching the
//! TS contract that a logging failure can't break the audited action.
//!
//! [`audit_entries`] is the read half — the admin Activity tab's
//! newest-first tail-read across shards.

use libsql::params::IntoValue;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::db::Db;
use crate::error::Result;
use crate::media::CacheBackend;
use crate::util::audit_shard_key;

#[derive(Debug, Deserialize, Serialize)]
struct AuditEntry {
    /// unix seconds
    at: i64,
    actor_user_id: Option<i64>,
    action: String,
    target_type: Option<String>,
    target_id: Option<String>,
    detail: Option<String>,
}

/// `audit(actor, action, targetType?, targetId?, detail?)` — never fails.
pub async fn audit(
    cache: &CacheBackend,
    actor_user_id: Option<i64>,
    action: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    detail: Option<&str>,
) {
    if let Err(e) = append(cache, actor_user_id, action, target_type, target_id, detail).await {
        tracing::warn!(error = %e, action, "[audit] write failed");
    }
}

async fn append(
    cache: &CacheBackend,
    actor_user_id: Option<i64>,
    action: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    detail: Option<&str>,
) -> anyhow::Result<()> {
    let at = crate::db::now();
    let entry = AuditEntry {
        at,
        actor_user_id,
        action: action.to_string(),
        target_type: target_type.map(str::to_string),
        target_id: target_id.map(str::to_string),
        detail: detail.map(str::to_string),
    };
    let line = serde_json::to_string(&entry)?;
    let key = audit_shard_key(at);

    // read → concat → put, same as the R2 facade (append isn't atomic).
    let prev = cache
        .read(&key)
        .await?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let mut text = prev;
    text.push_str(&line);
    text.push('\n');
    cache.put(&key, text.as_bytes(), None).await?;
    Ok(())
}

/* ------------------------------- reading ------------------------------- */

/// `TAIL_BYTES` — tail-read window per shard.
const TAIL_BYTES: u64 = 128 * 1024;
/// `MAX_SHARDS_BACK` — walk at most this many months for `limit` entries.
const MAX_SHARDS_BACK: i64 = 6;

/// `AuditRow` — the shape the admin Activity tab consumes (mirrors the
/// old D1 `audit_log` row): the entry fields plus a synthetic `id`,
/// resolved `actor_name`, and `created_at` aliased to `at`.
#[derive(Debug, Clone, Serialize)]
pub struct AuditRow {
    /// synthetic key for list rendering — `at * 1000 + index` (no row id
    /// exists in file storage)
    pub id: i64,
    /// unix seconds
    pub at: i64,
    pub created_at: i64,
    pub actor_user_id: Option<i64>,
    pub actor_name: Option<String>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub detail: Option<String>,
}

/// `auditEntries(limit)` — newest-first entries across monthly shards,
/// capped at `limit`. Actor names resolve with one `users` lookup —
/// reads happen only when the admin views the Activity tab, not per
/// event. Each shard is tail-read through the last [`TAIL_BYTES`]
/// window (`cacheHead` + ranged `cacheGet` in the TS original; the
/// `FsCache::open` seek here).
pub async fn audit_entries(db: &Db, cache: &CacheBackend, limit: usize) -> Result<Vec<AuditRow>> {
    let mut entries: Vec<AuditEntry> = Vec::new();
    let (base_y, base_m, _) = civil_from_days(crate::db::now().div_euclid(86_400));

    for back in 0..MAX_SHARDS_BACK {
        if entries.len() >= limit {
            break;
        }
        let key = audit_shard_key(month_start_unix(base_y, base_m, back));
        let Some(head) = cache.head(&key).await? else {
            continue;
        };
        if head.size == 0 {
            continue;
        }
        let start = head.size.saturating_sub(TAIL_BYTES);
        let Some(mut object) = cache.open(&key).await? else {
            continue;
        };
        object.stream.seek(std::io::SeekFrom::Start(start)).await?;
        let mut buf = Vec::new();
        object
            .stream
            .take(head.size - start)
            .read_to_end(&mut buf)
            .await?;
        let text = String::from_utf8_lossy(&buf);
        let mut parsed = parse_lines(&text, start > 0);
        parsed.reverse(); // newest-first within the shard
        entries.extend(parsed.into_iter().take(limit - entries.len()));
    }
    entries.truncate(limit);

    // One `users` lookup resolves every actor name, like the TS IN query.
    let mut ids: Vec<i64> = Vec::new();
    for e in &entries {
        if let Some(id) = e.actor_user_id {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    let mut names: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    if !ids.is_empty() {
        let placeholders = vec!["?"; ids.len()].join(", ");
        let sql = format!("SELECT id, name FROM users WHERE id IN ({placeholders})");
        let mut vals: Vec<libsql::Value> = Vec::with_capacity(ids.len());
        for id in &ids {
            vals.push((*id).into_value()?);
        }
        // Values are read inside `next()` — local-driver rows are bound to
        // the shared statement cursor, so a collect-then-read (`query_rows`)
        // sees only the final position.
        let mut rows = db.query_guard(&sql, libsql::params_from_iter(vals)).await?;
        while let Some(row) = rows.next().await? {
            names.insert(row.get::<i64>(0)?, row.get::<String>(1)?);
        }
    }

    Ok(entries
        .into_iter()
        .enumerate()
        .map(|(i, e)| AuditRow {
            id: e.at * 1000 + i as i64,
            at: e.at,
            created_at: e.at,
            actor_user_id: e.actor_user_id,
            actor_name: e.actor_user_id.and_then(|id| names.get(&id).cloned()),
            action: e.action,
            target_type: e.target_type,
            target_id: e.target_id,
            detail: e.detail,
        })
        .collect())
}

/// `parseLines` — split + parse each line, skipping the (possibly
/// partial) first line of a tail-read window and any corrupt JSON.
fn parse_lines(text: &str, skip_partial_first: bool) -> Vec<AuditEntry> {
    let body = if skip_partial_first {
        match text.find('\n') {
            Some(i) => &text[i + 1..],
            None => "",
        }
    } else {
        text
    };
    body.split('\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<AuditEntry>(line).ok())
        .collect()
}

/// Unix seconds of the 1st of the month `back` months before `(y, m)` —
/// the `Date.UTC(year, month - back, 1)` hop in audit.ts.
fn month_start_unix(y: i64, m: u32, back: i64) -> i64 {
    let total = y * 12 + (i64::from(m) - 1) - back;
    let (yy, mm) = (total.div_euclid(12), total.rem_euclid(12) + 1);
    days_from_civil(yy, mm as u32, 1) * 86_400
}

/// Civil date from unix days — same Howard-Hinnant algorithm as
/// `util::civil_from_days` (which stays private to `util`); duplicated
/// here so the month walk can compute shard keys for past months.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Inverse of [`civil_from_days`] — days since the unix epoch for a
/// civil date (Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = (y_adj - era * 400) as u64;
    let mp = if m > 2 {
        u64::from(m) - 3
    } else {
        u64::from(m) + 9
    };
    let doy = (153 * mp + 2) / 5 + u64::from(d - 1);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::now;
    use crate::media::FsCache;

    async fn test_db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("test.db").display());
        let db = Db::connect(&url).await.unwrap();
        db.migrate().await.unwrap();
        (db, dir)
    }

    #[test]
    fn month_walk_matches_audit_shard_keys() {
        // 2025-03 walk back past the year boundary.
        assert_eq!(
            audit_shard_key(month_start_unix(2025, 3, 0)),
            "audit/2025-03.jsonl"
        );
        assert_eq!(
            audit_shard_key(month_start_unix(2025, 3, 1)),
            "audit/2025-02.jsonl"
        );
        assert_eq!(
            audit_shard_key(month_start_unix(2025, 1, 1)),
            "audit/2024-12.jsonl"
        );
        assert_eq!(
            audit_shard_key(month_start_unix(2025, 1, 5)),
            "audit/2024-08.jsonl"
        );
    }

    #[test]
    fn parse_lines_skips_partial_and_corrupt() {
        let text = "{\"at\":1,\"actor_user_id\":null,\"action\":\"a\",\"target_type\":null,\"target_id\":null,\"detail\":null}\n{corrupt\n{\"at\":2,\"actor_user_id\":null,\"action\":\"b\",\"target_type\":null,\"target_id\":null,\"detail\":null}\n";
        // start > 0 → first (possibly truncated) line dropped entirely.
        let parsed = parse_lines(text, true);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].action, "b");
        // full read → corrupt middle line skipped, good lines kept.
        let parsed = parse_lines(text, false);
        assert_eq!(parsed.len(), 2);
    }

    #[tokio::test]
    async fn audit_entries_reads_newest_first_with_actor_names() {
        let (db, _d) = test_db().await;
        let dir = tempfile::tempdir().unwrap();
        let cache = CacheBackend::Fs(FsCache::new(dir.path()));
        db.execute(
            "INSERT INTO users (email, name, created_at, updated_at)
             VALUES ('a@x.co', 'Ann', 1, 1)",
            (),
        )
        .await
        .unwrap();
        let uid: i64 = db
            .query_guard("SELECT id FROM users", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap();

        for i in 0..5 {
            audit(&cache, Some(uid), &format!("act.{i}"), None, None, None).await;
        }
        audit(&cache, None, "anonymous", None, None, None).await;

        let rows = audit_entries(&db, &cache, 4).await.unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].action, "anonymous");
        assert_eq!(rows[0].actor_name, None);
        assert_eq!(rows[1].action, "act.4"); // newest-first across the cap
        assert_eq!(rows[3].action, "act.2");
        assert_eq!(rows[1].actor_name.as_deref(), Some("Ann"));
        assert_eq!(rows[1].actor_user_id, Some(uid));
        assert_eq!(rows[1].created_at, rows[1].at);
        // synthetic ids are unique (at * 1000 + index)
        let ids: std::collections::HashSet<i64> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), rows.len());

        // limit above available rows → everything, still newest-first
        let all = audit_entries(&db, &cache, 100).await.unwrap();
        assert_eq!(all.len(), 6);
        assert_eq!(all[5].action, "act.0");
    }

    #[tokio::test]
    async fn audit_entries_ignores_old_shards_and_empty_cache() {
        let (db, _d) = test_db().await;
        let dir = tempfile::tempdir().unwrap();
        let cache = CacheBackend::Fs(FsCache::new(dir.path()));

        // A shard older than MAX_SHARDS_BACK months is never read.
        let old_key = audit_shard_key(month_start_unix(2000, 1, 0));
        cache
            .put(&old_key, b"{\"at\":1,\"actor_user_id\":null,\"action\":\"ancient\",\"target_type\":null,\"target_id\":null,\"detail\":null}\n", None)
            .await
            .unwrap();
        assert!(audit_entries(&db, &cache, 100).await.unwrap().is_empty());

        // Sanity: shard key for `now` lines up with the walk base.
        let (y, m, _) = civil_from_days(now().div_euclid(86_400));
        assert_eq!(
            audit_shard_key(month_start_unix(y, m, 0)),
            audit_shard_key(now())
        );
    }
}

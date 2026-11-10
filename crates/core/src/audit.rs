//! Append-only audit trail — port of `src/lib/audit.ts`.
//!
//! Events are JSONL lines in monthly shards `audit/YYYY-MM.jsonl` inside
//! the media cache (R2 in the TS app; `FsCache` here — same key space, so
//! a future object-store backend slots in without touching callers).
//! `audit()` is best-effort: it logs and swallows failures, matching the
//! TS contract that a logging failure can't break the audited action.

use serde::Serialize;

use crate::media::FsCache;
use crate::util::audit_shard_key;

#[derive(Debug, Serialize)]
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
    cache: &FsCache,
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
    cache: &FsCache,
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

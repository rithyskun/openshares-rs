//! `GET|PATCH|POST /api/admin/cache` — port of
//! `src/app/api/admin/cache/route.ts` plus the `runCacheCleanup` half of
//! `src/lib/cache-cleanup.ts`: rotation TTL read/update and the manual
//! "run now" sweep. `FsCache::sweep` is the filesystem analogue of the
//! R2 list+delete batch; stats persist under `cache_last_cleanup` and
//! the run is audited as `cache.cleanup`.

use std::path::Path;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use openshares_core::audit::audit;
use openshares_core::db::{Db, now};
use openshares_core::error::Result as CoreResult;
use openshares_core::settings::{get_setting, set_setting};

use crate::body::{json_body, raw_body};
use crate::error::{ApiError, Result};
use crate::extract::AuthUser;
use crate::openapi::{CacheState, ErrorBody, PatchCacheResponse, RunCacheResponse};
use crate::state::AppState;

use super::{guard, mb};

/// `CACHE_TTL_KEY` / `CACHE_LAST_CLEANUP_KEY` / `DEFAULT_CACHE_TTL_DAYS`
/// / `MAX_CACHE_TTL_DAYS` — the cache-cleanup.ts constants.
const CACHE_TTL_KEY: &str = "cache_ttl_days";
const CACHE_LAST_CLEANUP_KEY: &str = "cache_last_cleanup";
const DEFAULT_CACHE_TTL_DAYS: i64 = 7;
const MAX_CACHE_TTL_DAYS: i64 = 90;

/// `CacheCleanupStats` — written under `cache_last_cleanup` and returned
/// by POST. Field names are camelCase like the TS object.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CacheCleanupStats {
    /// unix seconds — when the sweep ran.
    pub at: i64,
    /// Objects examined.
    pub scanned: u64,
    /// Objects evicted.
    pub deleted: u64,
    #[serde(rename = "freedBytes")]
    pub freed_bytes: u64,
    #[serde(rename = "ttlDays")]
    pub ttl_days: i64,
}

pub(super) fn router() -> Router<AppState> {
    Router::new().route(
        "/admin/cache",
        get(get_cache).patch(patch_cache).post(post_cache),
    )
}

/// `clampTtlDays` — `Math.min(90, Math.max(1, Math.round(n)))`.
/// `f64::round` rounds half away from zero vs JS toward +∞ — only
/// negative halves differ, and both clamp to 1.
fn clamp_ttl_days(n: f64) -> i64 {
    (n.round() as i64).clamp(1, MAX_CACHE_TTL_DAYS)
}

/// `cacheTtlDays` — `Number(value)`, finite && >= 1 else the default.
async fn cache_ttl_days(db: &Db) -> CoreResult<i64> {
    let stored = get_setting(db, CACHE_TTL_KEY).await?;
    let n = stored
        .as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<f64>().ok());
    Ok(match n {
        Some(n) if n.is_finite() && n >= 1.0 => clamp_ttl_days(n),
        _ => DEFAULT_CACHE_TTL_DAYS,
    })
}

/// `setCacheTtlDays` — clamp → persist → return the clamped value.
async fn set_cache_ttl_days(db: &Db, days: f64) -> CoreResult<i64> {
    let clamped = clamp_ttl_days(days);
    set_setting(db, CACHE_TTL_KEY, &clamped.to_string()).await?;
    Ok(clamped)
}

/// `lastCacheCleanup` — stats JSON or null (a malformed row reads as
/// null, like the TS try/catch).
async fn last_cache_cleanup(db: &Db) -> CoreResult<Option<CacheCleanupStats>> {
    let raw = get_setting(db, CACHE_LAST_CLEANUP_KEY).await?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
}

/// GET — `{ ttlDays, last }`.
#[utoipa::path(
    get,
    path = "/api/admin/cache",
    tag = "admin",
    responses(
        (status = 200, description = "Rotation TTL + last sweep stats", body = CacheState),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `settings:manage`", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn get_cache(State(state): State<AppState>, user: AuthUser) -> Result<Response> {
    guard(&state, &user.0, "settings:manage").await?;
    let ttl_days = cache_ttl_days(&state.db).await?;
    let last = last_cache_cleanup(&state.db).await?;
    Ok(Json(json!({ "ttlDays": ttl_days, "last": last })).into_response())
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct TtlBody {
    /// Number of days — JS `Number(v)` coercion accepts strings/bools;
    /// clamped to 1–90.
    #[serde(rename = "ttlDays")]
    ttl_days: Option<serde_json::Value>,
}

/// `Number(v)` — the JS coercion the PATCH applies to `body.ttlDays`:
/// numbers pass through, `Number("7")` → 7, `Number(null)` → 0,
/// non-coercible values → NaN — all funnelling into the same `< 1 ||
/// !isFinite` 400.
fn js_number(v: Option<&serde_json::Value>) -> f64 {
    use serde_json::Value;
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        _ => f64::NAN,
    }
}

/// PATCH — update the rotation TTL in days (clamped 1–90).
#[utoipa::path(
    patch,
    path = "/api/admin/cache",
    tag = "admin",
    request_body = TtlBody,
    responses(
        (status = 200, description = "TTL updated", body = PatchCacheResponse),
        (status = 400, description = "ttlDays not a finite number ≥ 1", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `settings:manage`", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn patch_cache(
    State(state): State<AppState>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "settings:manage").await?;
    let body = json_body::<TtlBody>(req, Some(user.0.sid.as_str())).await?;
    let n = js_number(body.ttl_days.as_ref());
    if !n.is_finite() || n < 1.0 {
        return Err(ApiError::bad_request(
            "ttlDays must be a number of days (1-90).",
        ));
    }
    let ttl_days = set_cache_ttl_days(&state.db, n).await?;
    audit(
        &state.cache,
        Some(user.0.id),
        "settings.cache_ttl.update",
        Some("settings"),
        Some("cache_ttl_days"),
        Some(&format!("{ttl_days}d")),
    )
    .await;
    Ok(Json(json!({ "ok": true, "ttlDays": ttl_days })).into_response())
}

/// POST — run the sweeper immediately (same code path as the daily
/// cron in the TS app). Signed empty body.
#[utoipa::path(
    post,
    path = "/api/admin/cache",
    tag = "admin",
    responses(
        (status = 200, description = "Sweep finished — stats recorded", body = RunCacheResponse),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `settings:manage`", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn post_cache(
    State(state): State<AppState>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "settings:manage").await?;
    raw_body(req, Some(user.0.sid.as_str())).await?;
    let stats = run_cache_cleanup(&state, Some(user.0.id), "manual").await?;
    Ok(Json(json!({ "ok": true, "stats": stats })).into_response())
}

/// `runCacheCleanup(env, { actorUserId, source })` — sweep objects older
/// than the TTL, record stats under `cache_last_cleanup`, audit the run.
/// `FsCache::sweep` covers the list+delete half; `scanned` is counted
/// with the same walk rules because sweep only reports deletions.
async fn run_cache_cleanup(
    state: &AppState,
    actor_user_id: Option<i64>,
    source: &str,
) -> CoreResult<CacheCleanupStats> {
    let ttl_days = cache_ttl_days(&state.db).await?;
    let scanned = count_cache_objects(Path::new(&state.config.media_cache_dir)).await;
    let (deleted, freed_bytes) = state
        .cache
        .sweep(Duration::from_secs(ttl_days.max(0) as u64 * 86_400))
        .await?;

    let stats = CacheCleanupStats {
        at: now(),
        scanned,
        deleted,
        freed_bytes,
        ttl_days,
    };
    let raw = serde_json::to_string(&stats)
        .map_err(|e| openshares_core::error::AppError::Internal(e.to_string()))?;
    set_setting(&state.db, CACHE_LAST_CLEANUP_KEY, &raw).await?;

    audit(
        &state.cache,
        actor_user_id,
        "cache.cleanup",
        Some("r2_bucket"),
        Some("openshares-cache"),
        Some(&format!(
            "{source}: evicted {deleted}/{scanned} ({}, ttl {ttl_days}d)",
            mb(freed_bytes)
        )),
    )
    .await;

    Ok(stats)
}

/// Count sweepable objects under `root` — the same traversal rules as
/// `FsCache::sweep` (top-level `audit/` skipped, `.meta.json` sidecars
/// don't count as objects) so `stats.scanned` matches the R2 `list`
/// total that cache-cleanup.ts reports.
async fn count_cache_objects(root: &Path) -> u64 {
    let mut count = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if dir.as_path() == root && name == "audit" {
                    continue; // audit shards are not cache
                }
                stack.push(path);
                continue;
            }
            if name.ends_with(".meta.json") {
                continue;
            }
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_clamp_bounds() {
        assert_eq!(clamp_ttl_days(0.0), 1);
        assert_eq!(clamp_ttl_days(0.4), 1);
        assert_eq!(clamp_ttl_days(0.6), 1);
        assert_eq!(clamp_ttl_days(7.0), 7);
        assert_eq!(clamp_ttl_days(90.4), 90);
        assert_eq!(clamp_ttl_days(91.0), 90);
        assert_eq!(clamp_ttl_days(1e20), 90);
    }

    #[test]
    fn js_number_coercion() {
        use serde_json::json;
        assert_eq!(js_number(Some(&json!(7))), 7.0);
        assert_eq!(js_number(Some(&json!("7"))), 7.0);
        assert_eq!(js_number(Some(&json!(" 7 "))), 7.0);
        assert_eq!(js_number(Some(&json!(true))), 1.0);
        assert!(js_number(Some(&json!(null))).is_nan());
        assert!(js_number(Some(&json!("abc"))).is_nan());
        assert!(js_number(Some(&json!({"a":1}))).is_nan());
        assert!(js_number(None).is_nan());
    }

    #[test]
    fn cleanup_stats_camel_case() {
        let stats = CacheCleanupStats {
            at: 1,
            scanned: 2,
            deleted: 3,
            freed_bytes: 4,
            ttl_days: 7,
        };
        let v = serde_json::to_value(&stats).unwrap();
        assert_eq!(v["freedBytes"], 4);
        assert_eq!(v["ttlDays"], 7);
        // round-trips like `lastCacheCleanup`'s JSON.parse
        let back: CacheCleanupStats = serde_json::from_value(v).unwrap();
        assert_eq!(back.deleted, 3);
    }
}

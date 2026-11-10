//! DB key-value settings — port of `src/lib/settings.ts` (the parts the
//! bot needs now; allowlist/cache-TTL helpers land with the API phase).

use std::sync::{Mutex, OnceLock};

use crate::db::{Db, now};
use crate::error::Result;

/// `getSetting` — `SELECT value FROM settings WHERE key = ?`.
pub async fn get_setting(db: &Db, key: &str) -> Result<Option<String>> {
    // Read inside `next()` — local-driver rows are bound to the shared
    // statement cursor (see `Db::conn` caveat).
    let mut rows = db
        .query_guard(
            "SELECT value FROM settings WHERE key = ?",
            libsql::params![key],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get::<String>(0)?)),
        None => Ok(None),
    }
}

/// `setSetting` — upsert on the `key` primary key.
pub async fn set_setting(db: &Db, key: &str, value: &str) -> Result<()> {
    db.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        libsql::params![key, value, now()],
    )
    .await?;
    Ok(())
}

/* ------------------------- bot username ------------------------- */

const BOT_USERNAME_KEY: &str = "bot_username";

/// `storedBotUsername` — last getMe result, persisted for public pages.
pub async fn stored_bot_username(db: &Db) -> Result<Option<String>> {
    get_setting(db, BOT_USERNAME_KEY).await
}

/// `rememberBotUsername`.
pub async fn remember_bot_username(db: &Db, username: &str) -> Result<()> {
    set_setting(db, BOT_USERNAME_KEY, username).await
}

/* ------------------------- site name ------------------------- */

const SITE_NAME_KEY: &str = "site_name";
pub const DEFAULT_SITE_NAME: &str = "OpenShares";
const SITE_NAME_MAX: usize = 80;
const SITE_NAME_TTL_SECS: i64 = 60;

static SITE_NAME_CACHE: OnceLock<Mutex<Option<(i64, String)>>> = OnceLock::new();

fn site_name_cache() -> &'static Mutex<Option<(i64, String)>> {
    SITE_NAME_CACHE.get_or_init(|| Mutex::new(None))
}

/// `siteName()` — display name, 60s module cache like the TS version.
pub async fn site_name(db: &Db) -> Result<String> {
    let ts = now();
    if let Ok(guard) = site_name_cache().lock() {
        if let Some((at, value)) = guard.as_ref() {
            if ts - at < SITE_NAME_TTL_SECS {
                return Ok(value.clone());
            }
        }
    }
    let stored = get_setting(db, SITE_NAME_KEY)
        .await?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let value = stored
        .map(|s| s.chars().take(SITE_NAME_MAX).collect::<String>())
        .unwrap_or_else(|| DEFAULT_SITE_NAME.to_string());
    if let Ok(mut guard) = site_name_cache().lock() {
        *guard = Some((ts, value.clone()));
    }
    Ok(value)
}

/// `setSiteName` — write + invalidate the cache.
pub async fn set_site_name(db: &Db, name: &str) -> Result<String> {
    let value: String = name.trim().chars().take(SITE_NAME_MAX).collect();
    let value = if value.is_empty() {
        DEFAULT_SITE_NAME.to_string()
    } else {
        value
    };
    set_setting(db, SITE_NAME_KEY, &value).await?;
    if let Ok(mut guard) = site_name_cache().lock() {
        *guard = None;
    }
    Ok(value)
}

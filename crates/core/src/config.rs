//! Env-only configuration — port of `src/lib/env.ts`.
//!
//! Secrets come from the process environment; nothing is hardcoded.
//! `dotenvy` loads `.env` for local dev (run binaries from the repo root);
//! in production the platform injects env vars.

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct Config {
    /// `BOT_TOKEN` — Telegram bot token. Optional at boot so the API can
    /// start during early phases; `TgClient` callers surface the error
    /// lazily, mirroring `requireBotToken()` in env.ts.
    pub bot_token: Option<String>,

    /// `APP_URL` — public base URL (default `http://localhost:8080`).
    pub app_url: String,

    /// `DATABASE_URL` — local SQLite path (`sqlite://<path>`), the
    /// fallback when `TURSO_DB_URL` is unset.
    /// Local dev default: `sqlite://data/openshares.db`
    pub database_url: String,

    /// `TURSO_DB_URL` — remote libSQL endpoint (`libsql://*.turso.io` or
    /// `https://`). When set the API talks to Turso directly (the TS
    /// `@libsql/client` equivalent); `TURSO_AUTH_TOKEN` goes with it.
    pub turso_db_url: Option<String>,

    /// `TURSO_AUTH_TOKEN` — JWT for the remote database. Optional in the
    /// config shape so `TURSO_DB_URL`-only local libsql-server setups work.
    pub turso_auth_token: Option<String>,

    /// `MEDIA_CACHE_DIR` — filesystem media blob cache (default `./data/cache`).
    pub media_cache_dir: String,

    /// `CACHE_TTL_DAYS` — cache sweep TTL seed (settings table overrides).
    pub cache_ttl_days: u32,

    /// `PORT` — HTTP listen port (default 8080).
    pub port: u16,

    /// `WEB_ORIGIN` — Next.js server to proxy non-`/api` page traffic to
    /// (e.g. `http://127.0.0.1:3000`). `None` → no proxy (API-only mode).
    pub web_origin: Option<String>,
}

impl Config {
    /// Load config from the process environment. Missing optional values
    /// fall back to dev defaults; the app always boots.
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        Ok(Self {
            bot_token: env("BOT_TOKEN"),
            app_url: env("APP_URL")
                .unwrap_or_else(|| "http://localhost:8080".to_string())
                .trim_end_matches('/')
                .to_string(),
            database_url: env("DATABASE_URL")
                .unwrap_or_else(|| "sqlite://data/openshares.db".to_string()),
            turso_db_url: env("TURSO_DB_URL"),
            turso_auth_token: env("TURSO_AUTH_TOKEN"),
            media_cache_dir: env("MEDIA_CACHE_DIR").unwrap_or_else(|| "./data/cache".to_string()),
            cache_ttl_days: env("CACHE_TTL_DAYS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            port: env("PORT").and_then(|v| v.parse().ok()).unwrap_or(8080),
            web_origin: env("WEB_ORIGIN").map(|v| v.trim_end_matches('/').to_string()),
        })
    }
}

/// Trimmed-or-undefined env read — mirrors `env()` in env.ts.
fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

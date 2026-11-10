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

    /// `MEDIA_CACHE_DIR` — filesystem media blob cache root (default
    /// `./data/cache`); used when `MEDIA_CACHE_BACKEND=fs`.
    pub media_cache_dir: String,

    /// `MEDIA_CACHE_BACKEND` — `fs` (default) or `s3`/`r2` (S3-compatible
    /// object storage — the `MEDIA_CACHE` R2 binding counterpart).
    pub media_cache_backend: String,

    /// `R2_ACCOUNT_ID` — Cloudflare account id; builds the default
    /// `https://<account>.r2.cloudflarestorage.com` endpoint.
    pub r2_account_id: Option<String>,

    /// `R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY` — R2 API token pair
    /// (S3 `AccessKeyId`/`SecretAccessKey`).
    pub r2_access_key_id: Option<String>,
    pub r2_secret_access_key: Option<String>,

    /// `R2_BUCKET` — object-store bucket name (the TS `MEDIA_CACHE`
    /// binding points at the `openshares-cache` bucket).
    pub r2_bucket: Option<String>,

    /// `R2_ENDPOINT` — optional S3 endpoint override (other
    /// S3-compatible stores); defaults to the account's R2 endpoint.
    pub r2_endpoint: Option<String>,

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
            media_cache_backend: env("MEDIA_CACHE_BACKEND").unwrap_or_else(|| "fs".to_string()),
            r2_account_id: env("R2_ACCOUNT_ID"),
            r2_access_key_id: env("R2_ACCESS_KEY_ID"),
            r2_secret_access_key: env("R2_SECRET_ACCESS_KEY"),
            r2_bucket: env("R2_BUCKET"),
            r2_endpoint: env("R2_ENDPOINT"),
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

//! Shared application state — one instance, cloned into every handler via
//! `axum::extract::State`. Mirrors the per-request singletons the TS app
//! pulls from env (db facade, cache, jwt keys, telegram client).

use openshares_core::config::Config;
use openshares_core::crypto::jwt::SessionJwt;
use openshares_core::db::Db;
use openshares_core::media::CacheBackend;
use openshares_core::telegram::TgClient;
use std::sync::Arc;

use openshares_core::error::Result;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cache: CacheBackend,
    pub config: Config,
    /// `None` when `BOT_TOKEN` is unset — telegram-dependent handlers
    /// surface 503 rather than failing at boot (mirrors `requireBotToken`).
    pub tg: Option<TgClient>,
    /// `None` when JWT keys are unset — auth routes 503, everything else
    /// still boots (same laziness as the TS env helpers). `Arc` because
    /// `jsonwebtoken` keys aren't `Clone`.
    pub jwt: Option<Arc<SessionJwt>>,
}

impl AppState {
    /// Build state from env — does NOT migrate (main owns that step so it
    /// stays visible in logs).
    pub async fn from_env() -> Result<Self> {
        let config = Config::from_env()?;
        let db = Db::connect_from(&config).await?;
        let cache = CacheBackend::from_config(&config)?;
        let tg = config.bot_token.clone().map(TgClient::new);
        let jwt = match SessionJwt::from_env() {
            Ok(j) => Some(Arc::new(j)),
            Err(e) => {
                tracing::warn!(error = %e, "JWT keys not configured — auth routes disabled");
                None
            }
        };
        Ok(Self {
            db,
            cache,
            config,
            tg,
            jwt,
        })
    }

    /// Telegram client or a 503-shaped error — for handlers that can't
    /// operate without the Bot API.
    pub fn require_tg(&self) -> crate::error::Result<&TgClient> {
        self.tg.as_ref().ok_or_else(|| {
            openshares_core::error::AppError::Config("BOT_TOKEN is not set".to_string()).into()
        })
    }
}

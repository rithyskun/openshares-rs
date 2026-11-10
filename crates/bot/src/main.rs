//! Long-polling Telegram bot — dev only. Mirrors `scripts/bot-poll.ts`.
//!
//! NEVER run alongside an active webhook: Telegram delivers each update
//! to exactly one consumer, and a registered webhook starves getUpdates.

use openshares_core::bot::{self, BotContext};
use openshares_core::config::Config;
use openshares_core::db::Db;
use openshares_core::media::CacheBackend;
use openshares_core::telegram::TgClient;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    let token = config
        .bot_token
        .clone()
        .ok_or_else(|| anyhow::anyhow!("BOT_TOKEN is required for the bot poller"))?;

    let db = Db::connect_from(&config).await?;
    db.migrate().await?;

    let ctx = BotContext {
        db,
        tg: TgClient::new(token),
        app_url: config.app_url.clone(),
        cache: CacheBackend::from_config(&config)?,
    };

    // Smoke test — proves token + connectivity before polling.
    // TgResult's Err is a String (Bot API description), map to anyhow.
    let me = ctx
        .tg
        .get_me()
        .await
        .map_err(|e| anyhow::anyhow!("getMe failed: {e}"))?;
    tracing::info!(id = me.id, username = ?me.username, "bot authenticated");

    // getUpdates long-poll loop. `allowed_updates` must mirror
    // bot-poll.ts / the webhook registration or updates get dropped.
    let mut offset: i64 = 0;
    loop {
        let payload = serde_json::json!({
            "offset": offset,
            "timeout": 30,
            "allowed_updates": ["message", "channel_post", "callback_query"],
        });

        match ctx
            .tg
            .call::<Vec<bot::TgUpdate>>("getUpdates", &payload)
            .await
        {
            Ok(updates) => {
                for update in updates {
                    offset = update.update_id + 1;
                    bot::process_update(&ctx, update).await;
                }
            }
            Err(err) => {
                // Same policy as tgCall failures in bot-poll.ts: log and retry.
                tracing::warn!(error = %err, "getUpdates failed; retrying in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

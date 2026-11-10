//! Telegram-users repository — ports the `telegram_users` reads in
//! `src/app/api/profile/link/route.ts` (`GET` reports the linked
//! account's bot profile) and the joins used by [`super::friends`].
//!
//! The `upsertTelegramUser` write stays inline in `crate::bot` (it runs
//! on every update the bot processes — see handler.ts); this module only
//! covers what the `/api/*` surface needs.

use crate::db::Db;
use crate::error::Result;
use crate::models::TelegramUser;

/// `WHERE telegram_id = ?` — profile/link's `SELECT username,
/// first_name FROM telegram_users`. Ids are TEXT (Telegram user ids as
/// strings, same convention as `users.telegram_id`).
pub async fn get_by_id(db: &Db, telegram_id: &str) -> Result<Option<TelegramUser>> {
    db.query_opt::<TelegramUser>(
        "SELECT * FROM telegram_users WHERE telegram_id = ?",
        libsql::params![telegram_id],
    )
    .await
}

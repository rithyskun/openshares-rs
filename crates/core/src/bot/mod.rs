//! Telegram update processing — port of `src/bot/handler.ts`.
//!
//! `/start CODE`, `/link`, `/add`, `/stats`, media ingestion, album
//! commands (`/newalbum`, `/albums`, `/upload`, `/view`, `/share`,
//! `/unshare`, `/done`), multi-step flows in `bot_state`, inline
//! keyboards, `sendMediaGroup` deep links (`/start album_<token>`).
//!
//! `process_update` is called by BOTH the webhook route (`server`) and
//! the long-poll loop (`bot` bin) — the same sharing pattern as
//! `handler.ts` being imported by the webhook route and `bot-poll.ts`.

use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit::audit;
use crate::db::{Db, PUBLIC_LINK_TTL, now};
use crate::media::CacheBackend;
use crate::rbac::can;
use crate::settings::{DEFAULT_SITE_NAME, remember_bot_username, site_name, stored_bot_username};
use crate::telegram::{
    MediaGroupItem, MediaKind, PhotoSize, TgChat, TgClient, TgDocument, TgMessageOut, TgUser,
    TgVideo, display_name, escape_html, inline_kb, is_dead_file_error, is_streamable_size,
};
use crate::util::{iso_date, new_token, parse_int_prefix, pretty_bytes, random_slug, short_date};

// ── Bot API update types (handler.ts TgUpdate & friends) ────────────

#[derive(Debug, Clone, Deserialize)]
pub struct TgUpdate {
    pub update_id: i64,
    pub message: Option<TgMessage>,
    pub channel_post: Option<TgChannelPost>,
    pub callback_query: Option<TgCallbackQuery>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TgMessage {
    pub from: Option<TgUser>,
    pub chat: TgChat,
    pub text: Option<String>,
    pub caption: Option<String>,
    pub photo: Option<Vec<PhotoSize>>,
    pub video: Option<TgVideo>,
    pub document: Option<TgDocument>,
    pub media_group_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TgChannelPost {
    pub chat: TgChat,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TgCallbackQuery {
    pub id: String,
    pub from: TgUser,
    pub message: Option<TgMessageOut>,
    pub data: Option<String>,
}

/// Everything the handlers need — cheap to clone (db/client handles).
#[derive(Clone)]
pub struct BotContext {
    pub db: Db,
    pub tg: TgClient,
    pub app_url: String,
    pub cache: CacheBackend,
}

const HELP: &str = "Send me photos or videos and I'll store them so you can manage and share them from the web app.\n\nTap-friendly — no need to type commands:\n/albums - open your album menu (view, share, add photos — all buttons)\n/newalbum TITLE - create an album (or just /newalbum and reply with a title)\n\nPower commands:\n/upload N|TITLE - file next sends into an album\n/view N · /share N · /unshare N - act on album N from /albums\n/done - finish uploading / cancel input\n\nTip: select several photos at once and they become a private album automatically — the group's caption becomes its name, and you'll get buttons to manage it.\n\n/start CODE - accept a friend invite\n/link CODE - attach this chat to your web account\n/add CODE - become a friend of someone (one-time code)\n/stats - how much you have stored\n/help - this message";

// ── Row types (serde::Deserialize; column aliases match the SELECTs) ─

#[derive(Deserialize)]
struct LinkCodeRow {
    purpose: String,
    user_id: Option<i64>,
    used: i64,
    expiry_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlbumEntry {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub token: String,
    pub is_public: i64,
    pub public_expires_at: Option<i64>,
    pub owner_user_id: Option<i64>,
    pub items: i64,
}

#[derive(Deserialize)]
struct AlbumItem {
    id: i64,
    kind: String,
    file_id: String,
}

#[derive(Deserialize)]
struct BotStateRow {
    action: String,
    album_id: Option<i64>,
    updated_at: i64,
}

#[derive(Deserialize)]
struct NameTelegramRow {
    name: String,
    telegram_id: Option<String>,
}

#[derive(Deserialize)]
struct TitleRow {
    title: String,
}

#[derive(Deserialize)]
struct AlbumTokenRow {
    id: i64,
    title: String,
    is_public: i64,
    public_expires_at: Option<i64>,
    owner_name: Option<String>,
}

#[derive(Deserialize)]
struct GroupHitRow {
    album_id: i64,
    title: String,
}

#[derive(Deserialize)]
struct StatsRow {
    photos: i64,
    videos: i64,
    bytes: i64,
}

// ── reply / shared helpers ──────────────────────────────────────────

/// `reply()` — awaited send inside the update's lifecycle; failures are
/// logged, never thrown (same contract as handler.ts).
async fn reply(ctx: &BotContext, chat_id: impl serde::Serialize, text: &str, kb: Option<Value>) {
    let extra = kb.map(|k| json!({ "reply_markup": k }));
    if let Err(e) = ctx.tg.send_message(chat_id, text, extra.as_ref()).await {
        tracing::warn!(error = %e, "[bot] sendMessage failed");
    }
}

async fn need_link_text(ctx: &BotContext) -> anyhow::Result<String> {
    Ok(format!(
        "Link your account first — /link CODE from your {} dashboard.",
        escape_html(&site_name(&ctx.db).await?)
    ))
}

/// `upsertTelegramUser` — refresh the telegram_users profile row.
pub async fn upsert_telegram_user(db: &Db, from: &TgUser) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO telegram_users (telegram_id, username, first_name, last_name, updated_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(telegram_id) DO UPDATE SET
           username = excluded.username,
           first_name = excluded.first_name,
           last_name = excluded.last_name,
           updated_at = excluded.updated_at",
        libsql::params![
            from.id.to_string(),
            from.username.as_deref(),
            from.first_name.as_deref(),
            from.last_name.as_deref(),
            now()
        ],
    )
    .await?;
    Ok(())
}

/// `getLinkedUserId` — web account bound to this telegram id, if any.
pub async fn get_linked_user_id(db: &Db, telegram_id: i64) -> anyhow::Result<Option<i64>> {
    let row: Option<i64> = db
        .scalar_opt(
            "SELECT id FROM users WHERE telegram_id = ?",
            libsql::params![telegram_id.to_string()],
        )
        .await?;
    Ok(row)
}

// ── /start CODE — friend / link / login codes ───────────────────────

async fn handle_start_code(
    ctx: &BotContext,
    from: &TgUser,
    code: &str,
    chat_id: i64,
) -> anyhow::Result<()> {
    let row: Option<LinkCodeRow> = ctx
        .db
        .query_opt(
            "SELECT purpose, user_id, used, expiry_at FROM link_codes WHERE code = ?",
            libsql::params![code],
        )
        .await?;

    let Some(row) = row else {
        reply(
            ctx,
            chat_id,
            "That code isn't valid anymore. Ask for a fresh one on the website.",
            None,
        )
        .await;
        return Ok(());
    };
    if row.used != 0 {
        reply(
            ctx,
            chat_id,
            "That code isn't valid anymore. Ask for a fresh one on the website.",
            None,
        )
        .await;
        return Ok(());
    }
    if let Some(expiry) = row.expiry_at
        && expiry < now()
    {
        reply(
            ctx,
            chat_id,
            "That code expired. Ask for a fresh one on the website.",
            None,
        )
        .await;
        return Ok(());
    }

    if row.purpose == "friend" {
        let Some(owner_id) = row.user_id else {
            reply(
                ctx,
                chat_id,
                "That invite code is broken (no owner). Sorry!",
                None,
            )
            .await;
            return Ok(());
        };
        ctx.db
            .execute(
                "INSERT OR IGNORE INTO friend_links (owner_user_id, friend_telegram_id, created_at)
                 VALUES (?, ?, ?)",
                libsql::params![owner_id, from.id.to_string(), now()],
            )
            .await?;
        ctx.db
            .execute(
                "UPDATE link_codes SET used = 1 WHERE code = ?",
                libsql::params![code],
            )
            .await?;

        let owner: Option<NameTelegramRow> = ctx
            .db
            .query_opt(
                "SELECT name, telegram_id FROM users WHERE id = ?",
                libsql::params![owner_id],
            )
            .await?;
        let owner_name = owner
            .as_ref()
            .map(|o| o.name.clone())
            .unwrap_or_else(|| "your friend".into());
        let site = site_name(&ctx.db).await?;
        reply(
            ctx,
            chat_id,
            &format!(
                "Done! You're connected with <b>{}</b>. They can send you photos and videos from {} now.",
                escape_html(&owner_name),
                escape_html(&site)
            ),
            None,
        )
        .await;
        if let Some(tg) = owner.and_then(|o| o.telegram_id) {
            let site = site_name(&ctx.db).await?;
            reply(
                ctx,
                tg,
                &format!(
                    "🤝 <b>{}</b> accepted your {} invite and is now on your friends list.",
                    escape_html(&display_name(Some(from))),
                    escape_html(&site)
                ),
                None,
            )
            .await;
        }
        return Ok(());
    }

    if row.purpose == "link" {
        let Some(user_id) = row.user_id else {
            reply(
                ctx,
                chat_id,
                "That link code is broken (no owner). Sorry!",
                None,
            )
            .await;
            return Ok(());
        };
        link_user_to_telegram(ctx, user_id, from, chat_id).await?;
        ctx.db
            .execute(
                "UPDATE link_codes SET used = 1 WHERE code = ?",
                libsql::params![code],
            )
            .await?;
        return Ok(());
    }

    reply(ctx, chat_id, "Unknown code type.", None).await;
    Ok(())
}

/// `linkUserToTelegram` — attach this telegram account to a web account,
/// migrate ownerless media, then greet.
async fn link_user_to_telegram(
    ctx: &BotContext,
    user_id: i64,
    from: &TgUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    let existing = get_linked_user_id(&ctx.db, from.id).await?;
    if existing == Some(user_id) {
        reply(
            ctx,
            chat_id,
            "This Telegram account is already linked to that web account. 👍",
            None,
        )
        .await;
        return Ok(());
    }
    if existing.is_some() {
        let site = site_name(&ctx.db).await?;
        reply(
            ctx,
            chat_id,
            &format!(
                "This Telegram account is already linked to a different {} account. Unlink it on the website first.",
                escape_html(&site)
            ),
            None,
        )
        .await;
        return Ok(());
    }
    let bot_profile = format!(
        "@{}",
        from.username
            .clone()
            .unwrap_or_else(|| escape_html(&display_name(Some(from))))
    );
    ctx.db
        .execute(
            "UPDATE users SET telegram_id = ?, bot_profile = ?, updated_at = ? WHERE id = ?",
            libsql::params![from.id.to_string(), bot_profile, now(), user_id],
        )
        .await?;
    // Adopt any media this telegram user sent before linking.
    ctx.db
        .execute(
            "UPDATE media SET owner_user_id = ? WHERE telegram_id = ? AND owner_user_id IS NULL",
            libsql::params![user_id, from.id.to_string()],
        )
        .await?;
    let count: i64 = ctx
        .db
        .scalar(
            "SELECT COUNT(*) FROM media WHERE owner_user_id = ?",
            libsql::params![user_id],
        )
        .await?;
    reply(
        ctx,
        chat_id,
        &format!(
            "🎉 Linked! <b>{}</b> is now connected to this chat.\n\nSend me photos or videos any time - your library already has <b>{count}</b> item{}.",
            ctx.app_url.trim_start_matches("http://").trim_start_matches("https://"),
            if count == 1 { "" } else { "s" }
        ),
        None,
    )
    .await;
    Ok(())
}

// ── bot conversation state (bot_state table) ────────────────────────

async fn get_bot_state(db: &Db, tg_id: &str) -> anyhow::Result<Option<BotStateRow>> {
    let row: Option<BotStateRow> = db
        .query_opt(
            "SELECT action, album_id, updated_at FROM bot_state WHERE telegram_id = ?",
            libsql::params![tg_id],
        )
        .await?;
    Ok(row)
}

async fn set_bot_state(
    db: &Db,
    tg_id: &str,
    action: &str,
    album_id: Option<i64>,
) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO bot_state (telegram_id, action, album_id, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(telegram_id) DO UPDATE SET
           action = excluded.action, album_id = excluded.album_id, updated_at = excluded.updated_at",
        libsql::params![tg_id, action, album_id, now()],
    )
    .await?;
    Ok(())
}

async fn clear_bot_state(db: &Db, tg_id: &str) -> anyhow::Result<()> {
    db.execute(
        "DELETE FROM bot_state WHERE telegram_id = ?",
        libsql::params![tg_id],
    )
    .await?;
    Ok(())
}

// ── albums ──────────────────────────────────────────────────────────

const ALBUM_SELECT: &str =
    "SELECT a.id, a.slug, a.title, a.token, a.is_public, a.public_expires_at, a.owner_user_id,
            (SELECT COUNT(*) FROM media m WHERE m.album_id = a.id) AS items
       FROM albums a";

/// `listAlbums` — owner's albums newest-first with media counts.
async fn list_albums(db: &Db, owner_id: i64) -> anyhow::Result<Vec<AlbumEntry>> {
    let rows = db
        .query_all::<AlbumEntry>(
            &format!(
                "{ALBUM_SELECT} WHERE a.owner_user_id = ? ORDER BY a.created_at DESC, a.id DESC"
            ),
            libsql::params![owner_id],
        )
        .await?;
    Ok(rows)
}

/// `loadAlbum` — single album by id (any owner; callers check ownership).
async fn load_album(db: &Db, id: i64) -> anyhow::Result<Option<AlbumEntry>> {
    let row: Option<AlbumEntry> = db
        .query_opt(
            &format!("{ALBUM_SELECT} WHERE a.id = ?"),
            libsql::params![id],
        )
        .await?;
    Ok(row)
}

/// `/cmd N` → the Nth album in the owner's list (1-based).
async fn pick_album(
    ctx: &BotContext,
    owner_id: i64,
    arg: &str,
    rows: Option<&[AlbumEntry]>,
) -> anyhow::Result<Option<AlbumEntry>> {
    let Some(n) = parse_int_prefix(arg) else {
        return Ok(None);
    };
    if n < 1 {
        return Ok(None);
    }
    let owned;
    let rows = match rows {
        Some(r) => r,
        None => {
            owned = list_albums(&ctx.db, owner_id).await?;
            &owned
        }
    };
    Ok(rows.get((n - 1) as usize).cloned())
}

/// Shared INSERT for /newalbum, /upload TITLE and media-group drops.
/// Returns `(id, slug)` — the `RETURNING id` rowid plus the generated slug.
async fn insert_album_ctx(
    ctx: &BotContext,
    owner_id: i64,
    title: &str,
    audit_detail: &str,
) -> anyhow::Result<(i64, String)> {
    let slug = random_slug();
    let id: i64 = ctx
        .db
        .scalar(
            "INSERT INTO albums (slug, owner_user_id, title, token, is_public, views, created_at)
             VALUES (?, ?, ?, ?, 0, 0, ?) RETURNING id",
            libsql::params![slug.as_str(), owner_id, title, new_token(16), now()],
        )
        .await?;
    audit(
        &ctx.cache,
        Some(owner_id),
        "album.create",
        Some("album"),
        Some(&slug),
        Some(audit_detail),
    )
    .await;
    Ok((id, slug))
}

async fn create_album(
    ctx: &BotContext,
    owner_id: i64,
    title: &str,
    chat_id: i64,
) -> anyhow::Result<()> {
    let clean: String = title.trim().chars().take(200).collect();
    if clean.is_empty() {
        reply(
            ctx,
            chat_id,
            "Give the album a title, e.g. <code>/newalbum Summer Trip</code>",
            None,
        )
        .await;
        return Ok(());
    }
    let (id, slug) = insert_album_ctx(ctx, owner_id, &clean, &format!("{clean} (bot)")).await?;
    let kb = inline_kb(&[
        vec![crate::telegram::InlineButton::callback(
            "📤 Add photos",
            format!("a:p:{id}"),
        )],
        vec![crate::telegram::InlineButton::callback("« Albums", "a:ls")],
    ]);
    reply(
        ctx,
        chat_id,
        &format!(
            "📁 Album \"<b>{}</b>\" created — tap to fill it, or manage it at {}/album/{slug}",
            escape_html(&clean),
            ctx.app_url
        ),
        Some(kb),
    )
    .await;
    Ok(())
}

async fn fetch_album_items(db: &Db, album_id: i64) -> anyhow::Result<Vec<AlbumItem>> {
    let rows = db
        .query_all::<AlbumItem>(
            "SELECT id, kind, file_id FROM media WHERE album_id = ? AND missing = 0 ORDER BY id ASC LIMIT 51",
            libsql::params![album_id],
        )
        .await?;
    Ok(rows)
}

async fn dead_item_count(db: &Db, album_id: i64) -> anyhow::Result<i64> {
    let n: i64 = db
        .scalar(
            "SELECT COUNT(*) FROM media WHERE album_id = ? AND missing = 1",
            libsql::params![album_id],
        )
        .await?;
    Ok(n)
}

#[derive(Default, Clone, Copy)]
struct AlbumSendStats {
    sent: i64,
    failed: i64,
    dead: i64,
}

/// `sendAlbumItem` — send one row; mark missing when the file_id is dead.
async fn send_album_item(
    ctx: &BotContext,
    chat_id: i64,
    m: &AlbumItem,
    stats: &mut AlbumSendStats,
) {
    let res = if m.kind == "photo" {
        ctx.tg.send_photo(chat_id, &m.file_id, None).await
    } else {
        ctx.tg.send_video(chat_id, &m.file_id, None).await
    };
    match res {
        Ok(_) => stats.sent += 1,
        Err(e) => {
            tracing::warn!(error = %e, media_id = m.id, "[bot] album item send failed");
            if is_dead_file_error(&e) {
                let _ = ctx
                    .db
                    .execute(
                        "UPDATE media SET missing = 1 WHERE id = ?",
                        libsql::params![m.id],
                    )
                    .await;
                stats.dead += 1;
            } else {
                stats.failed += 1;
            }
        }
    }
}

/// `sendAlbumItems` — push media as groups of ≤10, per-item fallback.
async fn send_album_items(ctx: &BotContext, chat_id: i64, items: &[AlbumItem]) -> AlbumSendStats {
    let mut stats = AlbumSendStats::default();
    for chunk in items.chunks(10) {
        if chunk.len() == 1 {
            send_album_item(ctx, chat_id, &chunk[0], &mut stats).await;
            continue;
        }
        let media: Vec<MediaGroupItem> = chunk
            .iter()
            .map(|m| MediaGroupItem {
                kind: if m.kind == "photo" {
                    MediaKind::Photo
                } else {
                    MediaKind::Video
                },
                media: m.file_id.clone(),
            })
            .collect();
        match ctx.tg.send_media_group(chat_id, &media).await {
            Ok(_) => stats.sent += chunk.len() as i64,
            Err(e) => {
                tracing::warn!(error = %e, "[bot] sendMediaGroup failed; per-item fallback");
                for m in chunk {
                    send_album_item(ctx, chat_id, m, &mut stats).await;
                }
            }
        }
    }
    stats
}

// ── /upload · /view · /share · /unshare ─────────────────────────────

/// `uploadTarget` — `/upload [N|TITLE]`; arms album_upload state.
async fn upload_target(
    ctx: &BotContext,
    owner_id: i64,
    arg: &str,
    chat_id: i64,
    tg_id: &str,
) -> anyhow::Result<()> {
    if !can(&ctx.db, owner_id, "media:upload", None).await? {
        reply(
            ctx,
            chat_id,
            "🔒 Your account doesn't have upload permission on this vault.",
            None,
        )
        .await;
        return Ok(());
    }
    let rows = list_albums(&ctx.db, owner_id).await?;
    if rows.is_empty() {
        reply(
            ctx,
            chat_id,
            "No albums yet — create one first, e.g. <code>/newalbum Summer Trip</code>.",
            None,
        )
        .await;
        return Ok(());
    }
    if arg.is_empty() {
        reply_with_album_menu(ctx, owner_id, chat_id).await;
        return Ok(());
    }
    let album: AlbumEntry;
    if let Some(n) = parse_int_prefix(arg) {
        if n < 1 {
            reply(
                ctx,
                chat_id,
                &format!(
                    "No album \"{}\" — pick a number from /albums.",
                    escape_html(arg)
                ),
                None,
            )
            .await;
            return Ok(());
        }
        let Some(a) = rows.get((n - 1) as usize) else {
            reply(
                ctx,
                chat_id,
                &format!(
                    "No album \"{}\" — pick a number from /albums.",
                    escape_html(arg)
                ),
                None,
            )
            .await;
            return Ok(());
        };
        if !can(&ctx.db, owner_id, "album:edit", Some(owner_id)).await? {
            reply(
                ctx,
                chat_id,
                "🔒 Your account can't edit albums on this vault.",
                None,
            )
            .await;
            return Ok(());
        }
        album = a.clone();
    } else {
        // Non-numeric arg = new album title, created and targeted at once.
        if !can(&ctx.db, owner_id, "album:create", None).await? {
            reply(
                ctx,
                chat_id,
                "🔒 Your account can't create albums — ask an admin.",
                None,
            )
            .await;
            return Ok(());
        }
        let title: String = arg.chars().take(200).collect();
        let (id, _) = insert_album_ctx(ctx, owner_id, &title, &format!("{title} (bot)")).await?;
        album = AlbumEntry {
            id,
            slug: String::new(),
            title,
            token: String::new(),
            is_public: 0,
            public_expires_at: None,
            owner_user_id: Some(owner_id),
            items: 0,
        };
    }
    arm_upload(ctx, &album, chat_id, tg_id).await?;
    Ok(())
}

/// `/view N` — re-deliver the owner's album into this chat.
async fn view_album(
    ctx: &BotContext,
    owner_id: i64,
    arg: &str,
    chat_id: i64,
) -> anyhow::Result<()> {
    if arg.is_empty() {
        reply_with_album_menu(ctx, owner_id, chat_id).await;
        return Ok(());
    }
    let Some(album) = pick_album(ctx, owner_id, arg, None).await? else {
        reply(
            ctx,
            chat_id,
            "No album with that number — check /albums.",
            None,
        )
        .await;
        return Ok(());
    };
    deliver_album(ctx, &album, chat_id).await?;
    Ok(())
}

/// `deliverAlbum` — push an album's media + report undeliverables.
async fn deliver_album(ctx: &BotContext, album: &AlbumEntry, chat_id: i64) -> anyhow::Result<()> {
    let rows = fetch_album_items(&ctx.db, album.id).await?;
    if rows.is_empty() {
        let kb = inline_kb(&[
            vec![crate::telegram::InlineButton::callback(
                "📤 Add photos",
                format!("a:p:{}", album.id),
            )],
            vec![crate::telegram::InlineButton::callback("« Albums", "a:ls")],
        ]);
        reply(
            ctx,
            chat_id,
            &format!(
                "📂 <b>{}</b> is empty — tap 📤 Add photos to fill it.",
                escape_html(&album.title)
            ),
            Some(kb),
        )
        .await;
        return Ok(());
    }
    let fetched = rows.len() as i64;
    let items: Vec<AlbumItem> = rows.into_iter().take(50).collect();
    reply(
        ctx,
        chat_id,
        &format!(
            "📂 <b>{}</b> — {} item{}{}",
            escape_html(&album.title),
            items.len(),
            if items.len() == 1 { "" } else { "s" },
            if fetched > items.len() as i64 {
                format!(
                    "\n\nFirst 50 shown — full album: {}/album/{}",
                    ctx.app_url, album.slug
                )
            } else {
                String::new()
            }
        ),
        None,
    )
    .await;
    let stats = send_album_items(ctx, chat_id, &items).await;
    let lost = stats.failed + stats.dead;
    if lost > 0 {
        reply(
            ctx,
            chat_id,
            &format!(
                "⚠️ {lost} item{} couldn't be re-sent — the original Telegram file{} \
                 no longer reachable (bot changed or messages deleted). They've been flagged in your vault.",
                if lost == 1 { "" } else { "s" },
                if lost == 1 { " is" } else { "s are" }
            ),
            None,
        )
        .await;
    }
    Ok(())
}

/// `doShare` — enable (or reuse) the 7-day public link; hand back both.
async fn do_share(
    ctx: &BotContext,
    owner_id: i64,
    album: &AlbumEntry,
    chat_id: i64,
) -> anyhow::Result<bool> {
    if !can(&ctx.db, owner_id, "album:edit", Some(owner_id)).await? {
        reply(
            ctx,
            chat_id,
            "🔒 Your account can't edit albums on this vault.",
            None,
        )
        .await;
        return Ok(false);
    }
    let expired = album.public_expires_at.is_some_and(|e| e <= now());
    let fresh = album.is_public == 0 || expired;
    let expires_at = if fresh {
        let e = now() + PUBLIC_LINK_TTL;
        ctx.db
            .execute(
                "UPDATE albums SET is_public = 1, public_expires_at = ? WHERE id = ?",
                libsql::params![e, album.id],
            )
            .await?;
        Some(e)
    } else {
        album.public_expires_at
    };
    let username = get_bot_username(ctx).await;
    let until = expires_at.map(short_date);
    reply(
        ctx,
        chat_id,
        &format!(
            "🔗 \"<b>{}</b>\" is shareable{}.\n\n{}\n\nWeb:\n{}/a/{}\n\nRecipients get the photos right inside Telegram.",
            escape_html(&album.title),
            until.map(|u| format!(" until <b>{u}</b>")).unwrap_or_default(),
            username
                .map(|u| format!("Telegram — forward this:\nhttps://t.me/{u}?start=album_{}\n", album.token))
                .unwrap_or_default(),
            ctx.app_url,
            album.token
        ),
        None,
    )
    .await;
    Ok(true)
}

async fn share_album(
    ctx: &BotContext,
    owner_id: i64,
    arg: &str,
    chat_id: i64,
) -> anyhow::Result<()> {
    let Some(album) = pick_album(ctx, owner_id, arg, None).await? else {
        reply(
            ctx,
            chat_id,
            "No album with that number — check /albums.",
            None,
        )
        .await;
        return Ok(());
    };
    do_share(ctx, owner_id, &album, chat_id).await?;
    Ok(())
}

/// `doUnshare` — kill public sharing; existing links die immediately.
async fn do_unshare(
    ctx: &BotContext,
    owner_id: i64,
    album: &AlbumEntry,
    chat_id: i64,
) -> anyhow::Result<bool> {
    if !can(&ctx.db, owner_id, "album:edit", Some(owner_id)).await? {
        reply(
            ctx,
            chat_id,
            "🔒 Your account can't edit albums on this vault.",
            None,
        )
        .await;
        return Ok(false);
    }
    if album.is_public == 0 {
        reply(
            ctx,
            chat_id,
            &format!(
                "\"<b>{}</b>\" isn't shared right now.",
                escape_html(&album.title)
            ),
            None,
        )
        .await;
        return Ok(false);
    }
    ctx.db
        .execute(
            "UPDATE albums SET is_public = 0, public_expires_at = NULL WHERE id = ?",
            libsql::params![album.id],
        )
        .await?;
    reply(
        ctx,
        chat_id,
        &format!(
            "Sharing off — old links to \"<b>{}</b>\" are dead.",
            escape_html(&album.title)
        ),
        None,
    )
    .await;
    Ok(true)
}

async fn unshare_album(
    ctx: &BotContext,
    owner_id: i64,
    arg: &str,
    chat_id: i64,
) -> anyhow::Result<()> {
    let Some(album) = pick_album(ctx, owner_id, arg, None).await? else {
        reply(
            ctx,
            chat_id,
            "No album with that number — check /albums.",
            None,
        )
        .await;
        return Ok(());
    };
    do_unshare(ctx, owner_id, &album, chat_id).await?;
    Ok(())
}

// ── tap-friendly album menus + callbacks ────────────────────────────

/// `albumMenuKb` — one button row per album + "New album".
fn album_menu_kb(rows: &[AlbumEntry]) -> Value {
    let mut buttons: Vec<Vec<crate::telegram::InlineButton>> = rows
        .iter()
        .map(|a| {
            vec![crate::telegram::InlineButton::callback(
                format!(
                    "📂 {} · {}{}",
                    if a.title.is_empty() {
                        "Untitled"
                    } else {
                        &a.title
                    },
                    a.items,
                    if a.is_public == 1 { " 🔗" } else { "" }
                ),
                format!("a:o:{}", a.id),
            )]
        })
        .collect();
    buttons.push(vec![crate::telegram::InlineButton::callback(
        "➕ New album",
        "a:new",
    )]);
    inline_kb(&buttons)
}

async fn reply_with_album_menu(ctx: &BotContext, owner_id: i64, chat_id: i64) {
    let rows = list_albums(&ctx.db, owner_id).await.unwrap_or_default();
    if rows.is_empty() {
        reply(
            ctx,
            chat_id,
            "No albums yet — tap below to create one.",
            Some(inline_kb(&[vec![crate::telegram::InlineButton::callback(
                "➕ New album",
                "a:new",
            )]])),
        )
        .await;
        return;
    }
    reply(
        ctx,
        chat_id,
        "📁 <b>Your albums</b> — tap to manage:",
        Some(album_menu_kb(&rows)),
    )
    .await;
}

/// `albumCard` — one album's action card (edited in place when possible).
async fn album_card(ctx: &BotContext, chat_id: i64, message_id: Option<i64>, album: &AlbumEntry) {
    let expired = album.public_expires_at.is_some_and(|e| e <= now());
    let is_pub = album.is_public == 1 && !expired;
    let until = if is_pub {
        album
            .public_expires_at
            .map(|e| format!(" until {}", short_date(e)))
    } else {
        None
    };
    let text = format!(
        "📂 <b>{}</b>\n{} item{} · {}",
        escape_html(if album.title.is_empty() {
            "Untitled"
        } else {
            &album.title
        }),
        album.items,
        if album.items == 1 { "" } else { "s" },
        if is_pub {
            format!("🔗 shared{}", until.unwrap_or_default())
        } else {
            "private".to_string()
        }
    );
    let share_btn = if is_pub {
        crate::telegram::InlineButton::callback("🚫 Stop sharing", format!("a:u:{}", album.id))
    } else {
        crate::telegram::InlineButton::callback("🔗 Share 7 days", format!("a:s:{}", album.id))
    };
    let kb = inline_kb(&[
        vec![
            crate::telegram::InlineButton::callback("👁 View", format!("a:v:{}", album.id)),
            crate::telegram::InlineButton::callback("📤 Add photos", format!("a:p:{}", album.id)),
        ],
        vec![share_btn],
        vec![crate::telegram::InlineButton::callback(
            "« All albums",
            "a:ls",
        )],
    ]);
    if let Some(mid) = message_id {
        let extra = json!({ "reply_markup": kb });
        match ctx
            .tg
            .edit_message_text(chat_id, mid, &text, Some(&extra))
            .await
        {
            Ok(_) => return,
            Err(e) if e.contains("not modified") => return,
            Err(e) => tracing::warn!(error = %e, "[bot] albumCard edit failed"),
        }
    }
    reply(ctx, chat_id, &text, Some(kb)).await;
}

/// `armUpload` — set album_upload state + prompt with Done/Cancel buttons.
async fn arm_upload(
    ctx: &BotContext,
    album: &AlbumEntry,
    chat_id: i64,
    tg_id: &str,
) -> anyhow::Result<()> {
    set_bot_state(&ctx.db, tg_id, "album_upload", Some(album.id)).await?;
    reply(
        ctx,
        chat_id,
        &format!(
            "📤 Collecting for \"<b>{}</b>\" ({} item{} so far).\n\nAttach photos or videos with the 📎 button below — I'll file each one until you tap Done.",
            escape_html(&album.title),
            album.items,
            if album.items == 1 { "" } else { "s" }
        ),
        Some(inline_kb(&[vec![
            crate::telegram::InlineButton::callback("✅ Done", "a:done"),
            crate::telegram::InlineButton::callback("❌ Cancel", "a:cx"),
        ]])),
    )
    .await;
    Ok(())
}

/// `finishPending` — shared /done + ✅-button finish.
async fn finish_pending(ctx: &BotContext, tg_id: &str, chat_id: i64) -> anyhow::Result<()> {
    let Some(state) = get_bot_state(&ctx.db, tg_id).await? else {
        reply(ctx, chat_id, "Nothing pending.", None).await;
        return Ok(());
    };
    clear_bot_state(&ctx.db, tg_id).await?;
    if state.action == "album_upload"
        && let Some(album_id) = state.album_id
    {
        let title: Option<String> = ctx
            .db
            .scalar_opt(
                "SELECT title FROM albums WHERE id = ?",
                libsql::params![album_id],
            )
            .await?;
        let added: i64 = ctx
            .db
            .scalar(
                "SELECT COUNT(*) FROM media WHERE album_id = ? AND created_at >= ?",
                libsql::params![album_id, state.updated_at],
            )
            .await?;
        reply(
            ctx,
            chat_id,
            &format!(
                "Done — added {added} item{} to \"<b>{}</b>\". Uploads go to your library again.",
                if added == 1 { "" } else { "s" },
                escape_html(title.as_deref().unwrap_or("album"))
            ),
            None,
        )
        .await;
        return Ok(());
    }
    reply(
        ctx,
        chat_id,
        "Done — new uploads go to your library again.",
        None,
    )
    .await;
    Ok(())
}

/// `handleCallback` — route a tapped inline button (`a:<act>[:<albumId>]`).
async fn handle_callback(ctx: &BotContext, cb: &TgCallbackQuery) -> anyhow::Result<()> {
    let data = cb.data.clone().unwrap_or_default();
    let chat_id = cb.message.as_ref().map(|m| m.chat.id).unwrap_or(cb.from.id);
    let msg_id = cb.message.as_ref().map(|m| m.message_id);
    let tg_id = cb.from.id.to_string();
    if !data.starts_with("a:") {
        let _ = ctx.tg.answer_callback_query(&cb.id, None).await;
        return Ok(());
    }
    let parts: Vec<&str> = data.split(':').collect();
    let act = parts.get(1).copied().unwrap_or("");
    let id_str = parts.get(2).copied();
    let owner = get_linked_user_id(&ctx.db, cb.from.id).await?;
    if owner.is_none() && act != "new" {
        let _ = ctx
            .tg
            .answer_callback_query(&cb.id, Some("Link your account first"))
            .await;
        reply(ctx, chat_id, &need_link_text(ctx).await?, None).await;
        return Ok(());
    }

    match act {
        "ls" => {
            let _ = ctx.tg.answer_callback_query(&cb.id, None).await;
            let rows = list_albums(&ctx.db, owner.unwrap_or(0)).await?;
            let text = "📁 <b>Your albums</b> — tap to manage:";
            let kb = album_menu_kb(&rows);
            if let Some(mid) = msg_id {
                let extra = json!({ "reply_markup": kb });
                match ctx
                    .tg
                    .edit_message_text(chat_id, mid, text, Some(&extra))
                    .await
                {
                    Ok(_) => {}
                    Err(e) if e.contains("not modified") => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "[bot] album list edit failed");
                        reply(ctx, chat_id, text, Some(kb)).await;
                    }
                }
            } else {
                reply_with_album_menu(ctx, owner.unwrap_or(0), chat_id).await;
            }
            return Ok(());
        }
        "new" => {
            let allowed = match owner {
                Some(o) => can(&ctx.db, o, "album:create", None).await?,
                None => false,
            };
            if !allowed {
                let _ = ctx
                    .tg
                    .answer_callback_query(
                        &cb.id,
                        Some(if owner.is_some() {
                            "No album permission"
                        } else {
                            "Link your account first"
                        }),
                    )
                    .await;
                if owner.is_none() {
                    reply(ctx, chat_id, &need_link_text(ctx).await?, None).await;
                }
                return Ok(());
            }
            let _ = ctx.tg.answer_callback_query(&cb.id, None).await;
            set_bot_state(&ctx.db, &tg_id, "album_title", None).await?;
            reply(
                ctx,
                chat_id,
                "What should I call it? Send the album title — /cancel to stop.",
                None,
            )
            .await;
            return Ok(());
        }
        "done" | "cx" => {
            let _ = ctx.tg.answer_callback_query(&cb.id, None).await;
            finish_pending(ctx, &tg_id, chat_id).await?;
            return Ok(());
        }
        _ => {}
    }

    // Album-scoped actions — the tapper must own the album.
    let album_id = id_str.and_then(parse_int_prefix);
    let album = match album_id {
        Some(id) => load_album(&ctx.db, id).await?,
        None => None,
    };
    let Some(album) = album else {
        let _ = ctx
            .tg
            .answer_callback_query(&cb.id, Some("Album not found"))
            .await;
        return Ok(());
    };
    let Some(owner_id) = owner else {
        let _ = ctx
            .tg
            .answer_callback_query(&cb.id, Some("Album not found"))
            .await;
        return Ok(());
    };
    if album.owner_user_id != Some(owner_id) {
        let _ = ctx
            .tg
            .answer_callback_query(&cb.id, Some("Album not found"))
            .await;
        return Ok(());
    }

    match act {
        "o" => {
            let _ = ctx.tg.answer_callback_query(&cb.id, None).await;
            album_card(ctx, chat_id, msg_id, &album).await;
        }
        "v" => {
            let _ = ctx.tg.answer_callback_query(&cb.id, Some("Sending…")).await;
            deliver_album(ctx, &album, chat_id).await?;
        }
        "s" => {
            let _ = ctx.tg.answer_callback_query(&cb.id, None).await;
            if do_share(ctx, owner_id, &album, chat_id).await? {
                let mut updated = album.clone();
                updated.is_public = 1;
                album_card(ctx, chat_id, msg_id, &updated).await;
            }
        }
        "u" => {
            let _ = ctx.tg.answer_callback_query(&cb.id, None).await;
            if do_unshare(ctx, owner_id, &album, chat_id).await? {
                let mut updated = album.clone();
                updated.is_public = 0;
                updated.public_expires_at = None;
                album_card(ctx, chat_id, msg_id, &updated).await;
            }
        }
        "p" => {
            if !can(&ctx.db, owner_id, "media:upload", None).await?
                || !can(&ctx.db, owner_id, "album:edit", Some(owner_id)).await?
            {
                let _ = ctx
                    .tg
                    .answer_callback_query(&cb.id, Some("No upload permission"))
                    .await;
                return Ok(());
            }
            let _ = ctx.tg.answer_callback_query(&cb.id, None).await;
            arm_upload(ctx, &album, chat_id, &tg_id).await?;
        }
        _ => {
            let _ = ctx.tg.answer_callback_query(&cb.id, None).await;
        }
    }
    Ok(())
}

// ── /start album_<token> — public album deep link ───────────────────

async fn handle_album_view(ctx: &BotContext, token: &str, chat_id: i64) -> anyhow::Result<()> {
    let album: Option<AlbumTokenRow> = ctx
        .db
        .query_opt(
            "SELECT a.id, a.title, a.is_public, a.public_expires_at, u.name AS owner_name
               FROM albums a LEFT JOIN users u ON u.id = a.owner_user_id
              WHERE a.token = ?",
            libsql::params![token],
        )
        .await?;

    let Some(mut album) = album else {
        reply(
            ctx,
            chat_id,
            "That album link doesn't go anywhere — check the link and try again.",
            None,
        )
        .await;
        return Ok(());
    };

    if album.is_public != 0 && album.public_expires_at.is_some_and(|e| e <= now()) {
        ctx.db
            .execute(
                "UPDATE albums SET is_public = 0 WHERE id = ?",
                libsql::params![album.id],
            )
            .await?;
        album.is_public = 0;
    }
    if album.is_public == 0 {
        reply(
            ctx,
            chat_id,
            "That album isn't shared anymore — ask the owner for a fresh link.",
            None,
        )
        .await;
        return Ok(());
    }

    let rows = fetch_album_items(&ctx.db, album.id).await?;
    if rows.is_empty() {
        reply(
            ctx,
            chat_id,
            &format!(
                "📂 <b>{}</b> is empty right now.",
                escape_html(&album.title)
            ),
            None,
        )
        .await;
        return Ok(());
    }
    let total = rows.len() as i64;
    let items: Vec<AlbumItem> = rows.into_iter().take(50).collect();

    let dead = dead_item_count(&ctx.db, album.id).await?;
    ctx.db
        .execute(
            "UPDATE albums SET views = views + 1 WHERE id = ?",
            libsql::params![album.id],
        )
        .await?;
    let site = site_name(&ctx.db).await?;
    reply(
        ctx,
        chat_id,
        &format!(
            "📂 <b>{}</b> — {} item{}\nShared by {} via {}{}",
            escape_html(&album.title),
            items.len(),
            if items.len() == 1 { "" } else { "s" },
            escape_html(album.owner_name.as_deref().unwrap_or("a friend")),
            escape_html(&site),
            if total > items.len() as i64 {
                format!("\n\nFull album on the web: {}/a/{token}", ctx.app_url)
            } else {
                String::new()
            }
        ),
        None,
    )
    .await;
    let stats = send_album_items(ctx, chat_id, &items).await;
    let lost = stats.failed + stats.dead + dead;
    if lost > 0 {
        reply(
            ctx,
            chat_id,
            &format!(
                "⚠️ {lost} item{} couldn't be delivered — the original Telegram file{} \
                 no longer reachable (the bot changed or the messages were deleted).\n\n\
                 View the album on the web: {}/a/{token}",
                if lost == 1 { "" } else { "s" },
                if lost == 1 { " is" } else { "s are" },
                ctx.app_url
            ),
            None,
        )
        .await;
    }
    Ok(())
}

// ── media ingestion ─────────────────────────────────────────────────

async fn handle_unlinked(ctx: &BotContext, from: &TgUser, chat_id: i64) -> anyhow::Result<()> {
    let site = site_name(&ctx.db).await?;
    reply(
        ctx,
        chat_id,
        &format!(
            "👋 Hi {}!\n\nI keep photos and videos safe inside Telegram. This vault is private — if the owner gave you access:\n\n1. Sign in at {}/login\n2. Send me:\n<code>/link CODE</code>\n(where CODE comes from your {} dashboard \"Link Telegram\")",
            escape_html(&display_name(Some(from))),
            ctx.app_url,
            escape_html(&site)
        ),
        None,
    )
    .await;
    Ok(())
}

fn pick_thumb(photo: &[PhotoSize]) -> Option<String> {
    // Largest size that still fits a thumbnail; smallest overall if all exceed it.
    let under: Vec<&PhotoSize> = photo.iter().filter(|p| p.width <= 400).collect();
    let best = if !under.is_empty() {
        under.into_iter().max_by_key(|p| p.width)
    } else {
        photo.iter().min_by_key(|p| p.width)
    };
    best.map(|p| p.file_id.clone())
}

struct InsertArgs {
    owner_id: i64,
    tg_id: String,
    caption: Option<String>,
    kind: &'static str,
    file_id: String,
    file_unique_id: Option<String>,
    thumb_file_id: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    duration: Option<i64>,
    mime: Option<String>,
    file_size: Option<i64>,
    availability: &'static str,
    album_id: Option<i64>,
    album_title: Option<String>,
    armed: bool,
    media_group_id: Option<String>,
    announce: bool,
    quiet: bool,
    chat_id: i64,
}

/// `insertAndConfirm` — index the media row, audit, and confirm in chat.
async fn insert_and_confirm(ctx: &BotContext, a: InsertArgs) -> anyhow::Result<()> {
    let slug = random_slug();
    ctx.db
        .execute(
            "INSERT INTO media
               (slug, owner_user_id, telegram_id, kind, file_id, file_unique_id, thumb_file_id,
                file_size, width, height, duration, mime, caption, shares_count, availability, created_at, album_id, media_group_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?)",
            libsql::params![
                slug.as_str(),
                a.owner_id,
                a.tg_id.as_str(),
                a.kind,
                a.file_id.as_str(),
                a.file_unique_id.as_deref(),
                a.thumb_file_id.as_deref(),
                a.file_size,
                a.width,
                a.height,
                a.duration,
                a.mime.as_deref(),
                a.caption.as_deref(),
                a.availability,
                now(),
                a.album_id,
                a.media_group_id.as_deref()
            ],
        )
        .await?;
    audit(
        &ctx.cache,
        Some(a.owner_id),
        "media.upload",
        Some("media"),
        Some(&slug),
        Some(&format!("bot:{}", a.kind)),
    )
    .await;

    let size = a
        .file_size
        .map(|n| format!(" ({})", pretty_bytes(n as u64)))
        .unwrap_or_default();
    let album_note = a
        .album_title
        .as_ref()
        .map(|t| format!(" → <b>{}</b>", escape_html(t)))
        .unwrap_or_default();
    let caption = a
        .caption
        .as_ref()
        .map(|c| format!("\n\n\"{}\"", escape_html(c)))
        .unwrap_or_default();
    let note = if a.availability == "telegram-only" {
        "\n\n⚠️ Over 20 MB - Telegram keeps it safe, but the web app can only share it here (Bot API download limit)."
    } else {
        ""
    };
    if a.announce {
        let kb = a.album_id.map(|id| {
            inline_kb(&[
                vec![
                    crate::telegram::InlineButton::callback("👁 View", format!("a:v:{id}")),
                    crate::telegram::InlineButton::callback("🔗 Share", format!("a:s:{id}")),
                ],
                vec![
                    crate::telegram::InlineButton::callback("📤 Add more", format!("a:p:{id}")),
                    crate::telegram::InlineButton::callback("« Albums", "a:ls"),
                ],
            ])
        });
        reply(
            ctx,
            a.chat_id,
            &format!(
                "📁 Started album \"<b>{}</b>\" — everything in this batch joins it.{note}\n\nPrivate for now — tap to manage:",
                escape_html(a.album_title.as_deref().unwrap_or(""))
            ),
            kb,
        )
        .await;
        return Ok(());
    }
    if a.quiet {
        return Ok(()); // batch member — the album announce already went out
    }
    let kb = if a.armed {
        Some(inline_kb(&[vec![
            crate::telegram::InlineButton::callback("✅ Done collecting", "a:done"),
            crate::telegram::InlineButton::callback("❌ Cancel", "a:cx"),
        ]]))
    } else {
        None
    };
    reply(
        ctx,
        a.chat_id,
        &format!(
            "✅ {} saved{size}{album_note}.{caption}{note}\n\nManage it at: {}/gallery",
            if a.kind == "photo" { "Photo" } else { "Video" },
            ctx.app_url
        ),
        kb,
    )
    .await;
    Ok(())
}

/// `handleIncomingMedia` — photo/video/document → media row (+album flows).
async fn handle_incoming_media(
    ctx: &BotContext,
    msg: &TgMessage,
    from: &TgUser,
    chat_id: i64,
) -> anyhow::Result<()> {
    let Some(owner) = get_linked_user_id(&ctx.db, from.id).await? else {
        handle_unlinked(ctx, from, chat_id).await?;
        return Ok(());
    };
    // RBAC before any indexing — suspended/viewer accounts can't fill the vault.
    if !can(&ctx.db, owner, "media:upload", None).await? {
        reply(
            ctx,
            chat_id,
            "🔒 Your account doesn't have upload permission on this vault. Ask an admin to grant the contributor role (or higher).",
            None,
        )
        .await;
        return Ok(());
    }

    // Pending /upload target files media into that album.
    let tg_id = from.id.to_string();
    let mut album_id: Option<i64> = None;
    let mut album_title: Option<String> = None;
    let mut armed = false;
    if let Some(state) = get_bot_state(&ctx.db, &tg_id).await?
        && state.action == "album_upload"
        && let Some(id) = state.album_id
    {
        let alb: Option<TitleRow> = ctx
            .db
            .query_opt(
                "SELECT title FROM albums WHERE id = ? AND owner_user_id = ?",
                libsql::params![id, owner],
            )
            .await?;
        if let Some(a) = alb {
            album_id = Some(id);
            album_title = Some(a.title);
            armed = true;
        } else {
            clear_bot_state(&ctx.db, &tg_id).await?; // stale pointer
        }
    }

    // A folder drop arrives as a media group: first item creates a private
    // album (caption → title); the rest find it via media_group_id.
    let mut announce_album = false;
    if album_id.is_none()
        && msg.media_group_id.is_some()
        && can(&ctx.db, owner, "album:create", None).await?
    {
        let group_id = msg.media_group_id.as_deref().unwrap_or("");
        let hit: Option<GroupHitRow> = ctx
            .db
            .query_opt(
                "SELECT m.album_id, a.title
                   FROM media m JOIN albums a ON a.id = m.album_id
                  WHERE m.media_group_id = ? AND m.owner_user_id = ?
                  LIMIT 1",
                libsql::params![group_id, owner],
            )
            .await?;
        if let Some(h) = hit {
            album_id = Some(h.album_id);
            let mut title = h.title;
            // Caption may ride on any member — rename an auto-titled album.
            if let Some(cap) = msg
                .caption
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
                && title.starts_with("Telegram drop ")
            {
                title = cap.chars().take(200).collect();
                ctx.db
                    .execute(
                        "UPDATE albums SET title = ? WHERE id = ?",
                        libsql::params![title.as_str(), h.album_id],
                    )
                    .await?;
            }
            album_title = Some(title);
        } else {
            let cap_title: String = msg
                .caption
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(|c| c.chars().take(200).collect())
                .unwrap_or_else(|| format!("Telegram drop {}", iso_date(now())));
            let (drop_id, _) =
                insert_album_ctx(ctx, owner, &cap_title, &format!("{cap_title} (bot drop)"))
                    .await?;
            album_id = Some(drop_id);
            album_title = Some(cap_title);
            announce_album = true;
        }
    }

    let photo = msg
        .photo
        .as_ref()
        .and_then(|p| p.iter().max_by_key(|s| s.width))
        .cloned();
    let is_photo = photo.is_some();
    let is_video = msg.video.is_some();
    let is_doc = msg
        .document
        .as_ref()
        .and_then(|d| d.mime_type.as_deref())
        .is_some_and(|m| m.starts_with("video/") || m.starts_with("image/"));
    if !is_photo && !is_video && !is_doc {
        if msg.chat.chat_type.as_deref() == Some("private") {
            reply(
                ctx,
                chat_id,
                "That file doesn't look like a photo or video — send it with the 📎 as a photo/video, or choose \"Send without compression\" as an image file.",
                None,
            )
            .await;
        }
        return Ok(());
    }

    let kind: &'static str = if is_photo {
        "photo"
    } else if is_video
        || msg
            .document
            .as_ref()
            .and_then(|d| d.mime_type.as_deref())
            .is_some_and(|m| m.starts_with("video/"))
    {
        "video"
    } else {
        "photo"
    };
    let file_id = photo
        .as_ref()
        .map(|p| p.file_id.clone())
        .or_else(|| msg.video.as_ref().map(|v| v.file_id.clone()))
        .or_else(|| msg.document.as_ref().map(|d| d.file_id.clone()))
        .unwrap_or_default();
    let file_unique_id = msg
        .video
        .as_ref()
        .and_then(|v| v.file_unique_id.clone())
        .or_else(|| msg.document.as_ref().and_then(|d| d.file_unique_id.clone()));
    let thumb_file_id = if is_photo {
        msg.photo.as_deref().and_then(pick_thumb)
    } else {
        msg.video
            .as_ref()
            .and_then(|v| v.thumbnail.as_ref().map(|t| t.file_id.clone()))
            .or_else(|| {
                msg.document
                    .as_ref()
                    .and_then(|d| d.thumbnail.as_ref().map(|t| t.file_id.clone()))
            })
    };
    let width = photo
        .as_ref()
        .map(|p| p.width)
        .or_else(|| msg.video.as_ref().map(|v| v.width))
        .or_else(|| {
            msg.document
                .as_ref()
                .and_then(|d| d.thumbnail.as_ref().map(|t| t.width))
        });
    let height = photo
        .as_ref()
        .map(|p| p.height)
        .or_else(|| msg.video.as_ref().map(|v| v.height))
        .or_else(|| {
            msg.document
                .as_ref()
                .and_then(|d| d.thumbnail.as_ref().map(|t| t.height))
        });
    let duration = msg.video.as_ref().map(|v| v.duration);
    let mime = if is_photo {
        Some("image/jpeg".to_string())
    } else {
        msg.video
            .as_ref()
            .and_then(|v| v.mime_type.clone())
            .or_else(|| msg.document.as_ref().and_then(|d| d.mime_type.clone()))
    };
    let file_size = photo
        .as_ref()
        .and_then(|p| p.file_size)
        .or_else(|| msg.video.as_ref().and_then(|v| v.file_size))
        .or_else(|| msg.document.as_ref().and_then(|d| d.file_size));
    // Warm the cache now (best effort); beyond Bot API reach → telegram-only.
    let availability = if is_streamable_size(file_size)
        && ctx.tg.ensure_cached(&file_id, &ctx.cache).await.is_some()
    {
        "available"
    } else {
        "telegram-only"
    };
    insert_and_confirm(
        ctx,
        InsertArgs {
            owner_id: owner,
            tg_id,
            caption: msg.caption.clone(),
            kind,
            file_id,
            file_unique_id,
            thumb_file_id,
            width,
            height,
            duration,
            mime,
            file_size: file_size.map(|v| v as i64),
            availability,
            album_id,
            album_title,
            armed,
            media_group_id: msg.media_group_id.clone(),
            announce: announce_album,
            quiet: msg.media_group_id.is_some() && !announce_album && !armed,
            chat_id,
        },
    )
    .await
}

// ── update dispatcher ───────────────────────────────────────────────

async fn process_inner(ctx: &BotContext, update: TgUpdate) -> anyhow::Result<()> {
    // channel_post arrives when the bot admins a channel (e.g. the storage
    // channel) — log it so admins can discover STORAGE_CHANNEL_ID.
    if let Some(post) = &update.channel_post {
        let chat = &post.chat;
        tracing::info!(
            chat_id = chat.id,
            title = ?chat.title,
            "[bot] channel_post — if this is your storage channel, set STORAGE_CHANNEL_ID={}",
            chat.id
        );
        return Ok(());
    }
    if let Some(cb) = &update.callback_query {
        upsert_telegram_user(&ctx.db, &cb.from).await?;
        handle_callback(ctx, cb).await?;
        return Ok(());
    }
    let Some(msg) = &update.message else {
        return Ok(());
    };
    let Some(from) = &msg.from else {
        return Ok(());
    };
    if from.is_bot.unwrap_or(false) {
        return Ok(());
    }
    upsert_telegram_user(&ctx.db, from).await?;
    let chat_id = msg.chat.id;

    // Media first (photos/videos arrive as plain messages)
    if msg.photo.is_some() || msg.video.is_some() || msg.document.is_some() {
        handle_incoming_media(ctx, msg, from, chat_id).await?;
        return Ok(());
    }

    let Some(text) = msg.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
        reply(ctx, chat_id, HELP, None).await;
        return Ok(());
    };

    if msg.chat.chat_type.as_deref() != Some("private") {
        return Ok(()); // group messages need no reply
    }

    let tg_id = from.id.to_string();
    let owner = get_linked_user_id(&ctx.db, from.id).await?;

    // In-flight flows: plain text answers a pending prompt instead of a command.
    let state = get_bot_state(&ctx.db, &tg_id).await?;
    if let Some(st) = &state
        && !text.starts_with('/')
    {
        if st.action == "album_title" {
            clear_bot_state(&ctx.db, &tg_id).await?;
            let allowed = match owner {
                Some(o) => can(&ctx.db, o, "album:create", None).await?,
                None => false,
            };
            if let Some(o) = owner {
                if allowed {
                    create_album(ctx, o, text, chat_id).await?;
                } else {
                    reply(
                        ctx,
                        chat_id,
                        "Link your account first, or ask an admin for album permission.",
                        None,
                    )
                    .await;
                }
            } else {
                reply(
                    ctx,
                    chat_id,
                    "Link your account first, or ask an admin for album permission.",
                    None,
                )
                .await;
            }
            return Ok(());
        }
        let title: Option<String> = match st.album_id {
            Some(id) => {
                ctx.db
                    .scalar_opt("SELECT title FROM albums WHERE id = ?", libsql::params![id])
                    .await?
            }
            None => None,
        };
        reply(
            ctx,
            chat_id,
            &format!(
                "📤 Still collecting for \"<b>{}</b>\" — send photos/videos, or /done to finish.",
                escape_html(title.as_deref().unwrap_or("your album"))
            ),
            None,
        )
        .await;
        return Ok(());
    }
    // Any command abandons a pending "name the album" prompt.
    if state.as_ref().is_some_and(|s| s.action == "album_title") {
        clear_bot_state(&ctx.db, &tg_id).await?;
    }

    let mut parts = text.split_whitespace();
    let raw_cmd = parts.next().unwrap_or("");
    let cmd = raw_cmd.split('@').next().unwrap_or("").to_lowercase();
    let arg = parts.collect::<Vec<_>>().join(" ");

    match cmd.as_str() {
        "/start" => {
            if let Some(rest) = arg.strip_prefix("album_") {
                handle_album_view(ctx, rest, chat_id).await?;
            } else if !arg.is_empty() {
                handle_start_code(ctx, from, &arg, chat_id).await?;
            } else {
                let site = site_name(&ctx.db).await?;
                reply(
                    ctx,
                    chat_id,
                    &format!("Welcome to {}! 👋\n\n{HELP}", escape_html(&site)),
                    None,
                )
                .await;
            }
        }
        "/link" | "/add" => {
            if arg.is_empty() {
                if cmd == "/link" {
                    let site = site_name(&ctx.db).await?;
                    reply(
                        ctx,
                        chat_id,
                        &format!(
                            "Usage: /link CODE - get the code on your {} dashboard.",
                            escape_html(&site)
                        ),
                        None,
                    )
                    .await;
                } else {
                    reply(
                        ctx,
                        chat_id,
                        "Usage: /add CODE - ask a friend for their share code.",
                        None,
                    )
                    .await;
                }
            } else {
                handle_start_code(ctx, from, &arg, chat_id).await?;
            }
        }
        "/stats" => {
            let Some(o) = owner else {
                reply(
                    ctx,
                    chat_id,
                    "Nothing yet - link your account first (see /help).",
                    None,
                )
                .await;
                return Ok(());
            };
            let row: StatsRow = ctx
                .db
                .query_one(
                    "SELECT
                       COALESCE(SUM(CASE WHEN kind = 'photo' THEN 1 ELSE 0 END), 0) AS photos,
                       COALESCE(SUM(CASE WHEN kind = 'video' THEN 1 ELSE 0 END), 0) AS videos,
                       COALESCE(SUM(file_size), 0) AS bytes
                     FROM media WHERE owner_user_id = ?",
                    libsql::params![o],
                )
                .await?;
            reply(
                ctx,
                chat_id,
                &format!(
                    "📚 Your library: {} photo{}, {} video{} - {} stored in Telegram.",
                    row.photos,
                    if row.photos == 1 { "" } else { "s" },
                    row.videos,
                    if row.videos == 1 { "" } else { "s" },
                    pretty_bytes(row.bytes.max(0) as u64)
                ),
                None,
            )
            .await;
        }
        "/newalbum" => {
            let Some(o) = owner else {
                reply(ctx, chat_id, &need_link_text(ctx).await?, None).await;
                return Ok(());
            };
            if !can(&ctx.db, o, "album:create", None).await? {
                reply(
                    ctx,
                    chat_id,
                    "🔒 Your account can't create albums — ask an admin.",
                    None,
                )
                .await;
                return Ok(());
            }
            if arg.is_empty() {
                set_bot_state(&ctx.db, &tg_id, "album_title", None).await?;
                reply(
                    ctx,
                    chat_id,
                    "What should I call it? Send the album title — /cancel to stop.",
                    None,
                )
                .await;
            } else {
                create_album(ctx, o, &arg, chat_id).await?;
            }
        }
        "/albums" => {
            let Some(o) = owner else {
                reply(ctx, chat_id, &need_link_text(ctx).await?, None).await;
                return Ok(());
            };
            reply_with_album_menu(ctx, o, chat_id).await;
        }
        "/upload" => {
            let Some(o) = owner else {
                reply(ctx, chat_id, &need_link_text(ctx).await?, None).await;
                return Ok(());
            };
            upload_target(ctx, o, &arg, chat_id, &tg_id).await?;
        }
        "/view" => {
            let Some(o) = owner else {
                reply(ctx, chat_id, &need_link_text(ctx).await?, None).await;
                return Ok(());
            };
            view_album(ctx, o, &arg, chat_id).await?;
        }
        "/share" => {
            let Some(o) = owner else {
                reply(ctx, chat_id, &need_link_text(ctx).await?, None).await;
                return Ok(());
            };
            share_album(ctx, o, &arg, chat_id).await?;
        }
        "/unshare" => {
            let Some(o) = owner else {
                reply(ctx, chat_id, &need_link_text(ctx).await?, None).await;
                return Ok(());
            };
            unshare_album(ctx, o, &arg, chat_id).await?;
        }
        "/done" | "/cancel" => {
            finish_pending(ctx, &tg_id, chat_id).await?;
        }
        "/help" => {
            reply(ctx, chat_id, HELP, None).await;
        }
        _ => {
            reply(ctx, chat_id, &format!("I didn't get that.\n\n{HELP}"), None).await;
        }
    }
    Ok(())
}

/// Process one update. Never throws so webhook/polling stay alive.
pub async fn process_update(ctx: &BotContext, update: TgUpdate) {
    if let Err(e) = process_inner(ctx, update).await {
        tracing::error!(error = %e, "[bot] update failed");
    }
}

/* ------------------------- bot username ------------------------- */

type UsernameCache = Mutex<Option<(i64, Option<String>)>>;
static BOT_USERNAME_CACHE: OnceLock<UsernameCache> = OnceLock::new();
const BOT_USERNAME_TTL_SECS: i64 = 600;

/// `getBotUsername` — stored value wins; getMe fallback; 10-min cache.
pub async fn get_bot_username(ctx: &BotContext) -> Option<String> {
    let ts = now();
    let cache = BOT_USERNAME_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some((at, u)) = guard.as_ref()
        && ts - at < BOT_USERNAME_TTL_SECS
    {
        return u.clone();
    }
    // Persisted username wins — avoids a Telegram round-trip per share.
    if let Ok(Some(stored)) = stored_bot_username(&ctx.db).await {
        if let Ok(mut guard) = cache.lock() {
            *guard = Some((ts, Some(stored.clone())));
        }
        return Some(stored);
    }
    let username = match ctx.tg.get_me().await {
        Ok(me) => me.username,
        Err(e) => {
            tracing::warn!(error = %e, "[bot] getMe failed");
            None
        }
    };
    if let Some(u) = &username {
        let _ = remember_bot_username(&ctx.db, u).await;
    }
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((ts, username.clone()));
    }
    username
}

/* ------------------- shared with the web API --------------------- */

/// Media fields `deliverShare` needs (web share routes build this).
pub struct ShareMedia {
    pub id: Option<i64>,
    pub kind: String,
    pub file_id: String,
    pub caption: Option<String>,
}

/// `deliverShare` — send someone's stored media to a friend's chat.
/// `Err` carries the Telegram error string (maps to the API's 502).
pub async fn deliver_share(
    ctx: &BotContext,
    media: &ShareMedia,
    to_telegram_id: &str,
    owner_name: &str,
) -> anyhow::Result<()> {
    let site = site_name(&ctx.db)
        .await
        .unwrap_or_else(|_| DEFAULT_SITE_NAME.to_string());
    let label = format!(
        "📤 From <b>{}</b> via {}",
        escape_html(owner_name),
        escape_html(&site)
    );
    let caption = media
        .caption
        .as_ref()
        .map(|c| format!("{label}\n\n{}", escape_html(c)))
        .unwrap_or(label);
    let extra = json!({ "caption": caption });
    let res = if media.kind == "photo" {
        ctx.tg
            .send_photo(to_telegram_id, &media.file_id, Some(&extra))
            .await
    } else {
        ctx.tg
            .send_video(to_telegram_id, &media.file_id, Some(&extra))
            .await
    };
    match res {
        Ok(_) => Ok(()),
        Err(e) => {
            if let Some(id) = media.id
                && is_dead_file_error(&e)
            {
                let _ = ctx
                    .db
                    .execute(
                        "UPDATE media SET missing = 1 WHERE id = ?",
                        libsql::params![id],
                    )
                    .await;
            }
            Err(anyhow::anyhow!(e))
        }
    }
}

/// `ExtractedMedia` — normalized media fields from any Telegram message
/// carrying photo/video/image-or-video document (bot intake + web uploads).
pub struct ExtractedMedia {
    pub kind: &'static str,
    pub file_id: String,
    pub file_unique_id: Option<String>,
    pub thumb_file_id: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<i64>,
    pub mime: Option<String>,
    pub file_size: Option<u64>,
    pub streamable: bool,
}

/// `extractMediaFromMessage`.
pub fn extract_media_from_message(msg: &TgMessage) -> Option<ExtractedMedia> {
    let photo = msg
        .photo
        .as_ref()
        .and_then(|p| p.iter().max_by_key(|s| s.width))
        .cloned();
    let video = msg.video.as_ref();
    let doc = msg.document.as_ref();
    let doc_mime = doc.and_then(|d| d.mime_type.as_deref()).unwrap_or("");
    let is_video_doc = doc_mime.starts_with("video/");
    let is_image_doc = doc_mime.starts_with("image/");
    if photo.is_none() && video.is_none() && !is_video_doc && !is_image_doc {
        return None;
    }

    let is_photo = photo.is_some() || (is_image_doc && video.is_none() && !is_video_doc);
    let kind: &'static str = if is_photo { "photo" } else { "video" };
    let file_id = photo
        .as_ref()
        .map(|p| p.file_id.clone())
        .or_else(|| video.map(|v| v.file_id.clone()))
        .or_else(|| doc.map(|d| d.file_id.clone()))
        .unwrap_or_default();
    let thumb_file_id = if photo.is_some() {
        msg.photo.as_deref().and_then(pick_thumb)
    } else {
        video
            .and_then(|v| v.thumbnail.as_ref().map(|t| t.file_id.clone()))
            .or_else(|| doc.and_then(|d| d.thumbnail.as_ref().map(|t| t.file_id.clone())))
    };
    let file_size = photo
        .as_ref()
        .and_then(|p| p.file_size)
        .or_else(|| video.and_then(|v| v.file_size))
        .or_else(|| doc.and_then(|d| d.file_size));
    Some(ExtractedMedia {
        kind,
        file_id,
        file_unique_id: video
            .and_then(|v| v.file_unique_id.clone())
            .or_else(|| doc.and_then(|d| d.file_unique_id.clone())),
        thumb_file_id,
        width: photo
            .as_ref()
            .map(|p| p.width)
            .or_else(|| video.map(|v| v.width))
            .or_else(|| doc.and_then(|d| d.thumbnail.as_ref().map(|t| t.width))),
        height: photo
            .as_ref()
            .map(|p| p.height)
            .or_else(|| video.map(|v| v.height))
            .or_else(|| doc.and_then(|d| d.thumbnail.as_ref().map(|t| t.height))),
        duration: video.map(|v| v.duration),
        mime: if photo.is_some() {
            Some("image/jpeg".to_string())
        } else {
            video
                .and_then(|v| v.mime_type.clone())
                .or_else(|| doc.and_then(|d| d.mime_type.clone()))
        },
        streamable: is_streamable_size(file_size),
        file_size,
    })
}

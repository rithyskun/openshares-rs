//! Database row types — one `serde::Deserialize` struct per table in
//! `crates/core/migrations/` (the SQLite schema copied verbatim from the
//! TypeScript app's `migrations/`). Rows decode through
//! `libsql::de::from_row` inside the `Db::query_*` helpers.
//!
//! API request/response DTOs are defined in `server` routes; DB entities
//! never cross the HTTP boundary — the same DTO/entity split as the
//! TypeScript codebase. Columns are 1:1 with the schema: `TEXT` →
//! `String`/`Option<String>`, `INTEGER` → `i64`/`Option<i64>` (flags stay
//! `i64`, timestamps are unix seconds).

// Field names match column names — `libsql::de::from_row` maps by name,
// same as the old `FromRow` derive did.

/* ------------------------- 0001_init.sql ------------------------- */

/// `users` — vault accounts. `disabled_at` arrived in `0003_rbac.sql`
/// (suspension), the rest is the original 0001 shape.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct User {
    pub id: i64,
    pub email: Option<String>,
    pub password_hash: Option<String>,
    pub name: String,
    pub telegram_id: Option<String>,
    pub bot_profile: Option<String>,
    /// 0/1 — per-user bot notification toggle.
    pub notifications: i64,
    /// Non-NULL = suspended; session lookup and writes reject.
    pub disabled_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// `sessions` — server-side revocation anchor for the RS256 cookie JWT.
/// `created_at` arrived in `0008_session_created.sql` (the "Signed in …"
/// stamp; NULL on pre-migration rows).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub user_id: i64,
    pub expires_at: i64,
    pub created_at: Option<i64>,
}

/// `telegram_users` — bot-side profile cache keyed by Telegram user id.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TelegramUser {
    pub telegram_id: String,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub updated_at: i64,
}

/// `link_codes` — one-time bot codes. `purpose` is 'login' | 'link' |
/// 'friend' (see the migration comment for the semantics).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LinkCode {
    pub code: String,
    pub purpose: String,
    pub user_id: Option<i64>,
    pub expiry_at: Option<i64>,
    pub used: i64,
    pub created_at: i64,
}

/// `friend_links` — owner → Telegram-friend grants for direct shares.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FriendLink {
    pub id: i64,
    pub owner_user_id: i64,
    pub friend_telegram_id: String,
    pub created_at: i64,
}

/// `media` — one stored photo/video. `media_group_id` (0006) groups a
/// multi-send; `missing` (0007) flags a dead `file_id` separately from the
/// `availability` CHECK constraint.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Media {
    pub id: i64,
    pub slug: String,
    pub owner_user_id: Option<i64>,
    /// Provenance: the sender's Telegram id, or 'upload' for web uploads.
    pub telegram_id: String,
    /// CHECK (kind IN ('photo','video')).
    pub kind: String,
    pub file_id: String,
    pub file_unique_id: Option<String>,
    pub thumb_file_id: Option<String>,
    pub file_size: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<i64>,
    pub mime: Option<String>,
    pub caption: Option<String>,
    pub shares_count: i64,
    /// CHECK (availability IN ('available','telegram-only')).
    pub availability: String,
    pub created_at: i64,
    /// NULL = vault (unfiled); SET NULL when the album is deleted.
    pub album_id: Option<i64>,
    pub media_group_id: Option<String>,
    /// 0/1 — the Telegram `file_id` no longer resolves.
    pub missing: i64,
}

/// `shares` — 'link' shares mint `/s/<token>` URLs; 'telegram' shares are
/// delivery receipts into a friend's chat. `views` (0010) and `likes`
/// (0011) are public-page counters.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Share {
    pub id: i64,
    pub media_id: i64,
    /// CHECK (kind IN ('telegram','link')).
    pub kind: String,
    pub token: Option<String>,
    pub to_telegram_id: Option<String>,
    pub created_by: Option<i64>,
    pub created_by_tg: Option<String>,
    /// NULL = no expiry; link shares get `created_at + PUBLIC_LINK_TTL`.
    pub expires_at: Option<i64>,
    pub created_at: i64,
    pub views: i64,
    pub likes: i64,
}

/// `albums` — named media collections. Public sharing is gated by
/// `is_public` + `public_expires_at` (the /a/<token> surface); `likes`
/// arrived in 0011.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Album {
    pub id: i64,
    pub slug: String,
    pub owner_user_id: Option<i64>,
    pub title: String,
    pub token: String,
    /// 0/1 — whether the /a/<token> link is live (with expiry below).
    pub is_public: i64,
    pub views: i64,
    pub created_at: i64,
    pub public_expires_at: Option<i64>,
    pub likes: i64,
}

/* ------------------------- 0002_rate_limits.sql ------------------------- */

/// `rate_limits` — deterministic fixed-window counter rows; `bucket` is
/// `"<LIMITER>:<key>"` (see `src/lib/security.ts` LIMITS).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RateLimit {
    pub bucket: String,
    pub window_start: i64,
    pub count: i64,
}

/* ------------------------- 0003_rbac.sql ------------------------- */

/// `roles` — built-in roles (`builtin = 1`: admin/editor/contributor/
/// viewer) can't be renamed or deleted.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Role {
    pub id: i64,
    pub name: String,
    pub builtin: i64,
    pub created_at: i64,
}

/// `role_permissions` — composite PK (role_id, permission_key); keys are
/// `resource:action[:scope]` or the `*` admin wildcard.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RolePermission {
    pub role_id: i64,
    pub permission_key: String,
}

/// `user_roles` — composite PK (user_id, role_id).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UserRole {
    pub user_id: i64,
    pub role_id: i64,
}

/* ------------------------- 0004_settings.sql ------------------------- */

/// `settings` — DB-managed key-value store (allowlist, site name, cache
/// TTL, last-cleanup stats, bot username).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub updated_at: i64,
}

/// `audit_log` — legacy D1-era table, kept in the schema for parity. The
/// live audit trail is JSONL shards in blob storage (`crate::audit`),
/// same as the TS `audit.ts`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AuditLog {
    pub id: i64,
    pub actor_user_id: Option<i64>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub detail: Option<String>,
    pub created_at: i64,
}

/* ------------------------- 0005_bot_state.sql ------------------------- */

/// `bot_state` — pending multi-step bot flows ('album_title',
/// 'album_upload'), keyed by Telegram user id.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BotState {
    pub telegram_id: String,
    pub action: String,
    pub album_id: Option<i64>,
    pub updated_at: i64,
}

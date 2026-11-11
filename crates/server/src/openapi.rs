//! OpenAPI spec + Swagger UI — the compile-time doc built from the
//! `#[utoipa::path]` annotations on every route handler.
//!
//! [`ApiDoc`] is only mounted when `SWAGGER_UI` is set (see `main.rs` /
//! `.env.example`). The `*Dto`/`*Body` types below exist for docs only —
//! handlers build their JSON with `serde_json::json!`, so these mirror
//! the wire shapes the TS routes defined (each handler's doc comment
//! names the shape it returns; keep the two in sync).

use std::collections::HashMap;

use serde::Serialize;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

use crate::routes::admin;

/// The generated OpenAPI document — every `/api/*` operation.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "OpenShares API",
        version = "0.1.0",
        description = "Private media vault API (axum port of the Next.js `/api/*` surface).\n\n\
            **Auth:** signed-in calls carry the `os_session` httpOnly cookie \
            (RS256 JWT anchored to a `sessions` row). Mutating calls additionally \
            need the `x-csrf-token` header echoing the `XSRF-TOKEN` cookie \
            (double-submit), and session-gated JSON bodies must be HMAC-signed \
            with the session's `signingKey` — `/api/me` returns it when \
            `REQUEST_SIGNING_SECRET` is configured. Public share/album surfaces \
            are token-gated instead of session-gated."
    ),
    servers((url = "/")),
    paths(
        // auth + account
        crate::routes::auth::login,
        crate::routes::auth::logout,
        crate::routes::auth::rsa_key,
        crate::routes::me::me,
        crate::routes::profile::change_password,
        crate::routes::profile::link_state,
        crate::routes::profile::link_create,
        // meta
        crate::routes::meta::meta,
        // media
        crate::routes::media_api::list_media,
        crate::routes::media_api::media_summary,
        crate::routes::media_api::move_media,
        crate::routes::media_api::list_share_links,
        crate::routes::media_api::create_share,
        crate::routes::media_api::revoke_share_link,
        crate::routes::media_stream::get_media,
        crate::routes::media_stream::patch_media,
        crate::routes::media_stream::delete_media,
        crate::routes::by_token::by_token,
        // albums
        crate::routes::albums::list_albums,
        crate::routes::albums::create_album,
        crate::routes::albums::get_album,
        crate::routes::albums::patch_album,
        crate::routes::albums::delete_album,
        crate::routes::albums::upload_album_file,
        // friends
        crate::routes::friends::list_friends,
        crate::routes::friends::create_invite,
        crate::routes::friends::remove_friend,
        // public
        crate::routes::public_share::public_share,
        crate::routes::public_album::public_album,
        crate::routes::public_album::like_album,
        crate::routes::public_album::like_share,
        // bot
        crate::routes::bot_webhook::bot_webhook,
        // admin
        admin::access::get_access,
        admin::access::add_access,
        admin::access::remove_access,
        admin::activity::get_activity,
        admin::cache::get_cache,
        admin::cache::patch_cache,
        admin::cache::post_cache,
        admin::permissions::get_permissions,
        admin::roles::list_roles,
        admin::roles::create_role,
        admin::roles::patch_role,
        admin::roles::delete_role,
        admin::settings::get_settings,
        admin::settings::patch_settings,
        admin::users::list_users,
        admin::users::create_user,
        admin::users::patch_user,
        admin::users::assign_role,
        admin::users::assign_role_put,
        admin::users::remove_role,
        admin::vault::vault,
    ),
    components(schemas(
        ErrorBody, OkBody,
        MetaResponse, LoginResponse, RsaKeyResponse, MeResponse,
        LinkState, LinkCreated,
        FriendDto, FriendsResponse, InviteBody, InviteResponse,
        AlbumDto, AlbumsResponse, CreateAlbumBody, AlbumCreated, PatchAlbumBody,
        PatchAlbumResponse, UploadForm, UploadResponse, UploadedItem,
        MediaItemDto, MediaListResponse, MediaSummary, ShareLinkDto,
        ShareLinksResponse, CreateShareBody, LinkShareCreated, TelegramShareSent,
        RevokeShareBody, MoveBody, MoveResponse, PatchMediaBody, PatchMediaResponse,
        PublicShareDto, PublicAlbumHead, PublicMediaItem, PublicAlbumResponse,
        LikeResponse,
        AdminAccessList, AdminAccessMut, ActivityResponse, AuditEntryDto,
        PermissionGroup, PermissionsResponse, RoleDto, RolesResponse,
        PatchRoleBody, RoleCreated, SettingsResponse, SettingsPatched,
        AdminUserDto, RoleRefDto, UsersResponse, PatchUserBody, UserCreated,
        CacheState, PatchCacheResponse, RunCacheResponse,
        CountBytes, AlbumCounts, ShareCounts, UserCounts, TopOwner, VaultStats,
    )),
    tags(
        (name = "auth", description = "Session sign-in/out and the login RSA key"),
        (name = "account", description = "Signed-in user identity, password, Telegram link"),
        (name = "meta", description = "Instance metadata"),
        (name = "media", description = "Vault media list, stream, share links, move"),
        (name = "albums", description = "Album CRUD + file upload"),
        (name = "friends", description = "Friend list + t.me invites"),
        (name = "public", description = "Token-gated share/album surfaces — no session"),
        (name = "bot", description = "Telegram webhook (secret-token header)"),
        (name = "admin", description = "Admin console routes — RBAC-gated"),
    ),
    modifiers(&SecurityAddon)
)]
pub struct ApiDoc;

/// Registers the two ApiKey schemes referenced by `security(...)` on the
/// path annotations.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session_cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "os_session",
                "httpOnly session JWT issued by POST /api/auth/login.",
            ))),
        );
        components.add_security_scheme(
            "webhook_secret",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "x-telegram-bot-api-secret-token",
                "Must equal the WEBHOOK_SECRET env var (constant-time compared).",
            ))),
        );
    }
}

/* ============================ shared bodies ============================ */

/// `{ "error": "…" }` — the error convention every route returns; a
/// `code` joins on the 401s that distinguish an expired session.
#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    /// Human-readable message (the TS route's exact text).
    pub error: String,
    /// `UNAUTHENTICATED` | `TOKEN_EXPIRED` — only on `/api/me` 401s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// `{ "ok": true }`.
#[derive(Serialize, ToSchema)]
pub struct OkBody {
    pub ok: bool,
}

/* ============================== auth ================================= */

/// `POST /api/auth/login` → `{ok, signingKey}` + `os_session` cookie.
#[derive(Serialize, ToSchema)]
pub struct LoginResponse {
    pub ok: bool,
    /// HMAC request-signing key — `null` when `REQUEST_SIGNING_SECRET`
    /// is unset.
    #[serde(rename = "signingKey")]
    pub signing_key: Option<String>,
}

/// `GET /api/auth/rsa-key` → `{enabled, publicKey, keyId}`; only
/// `{enabled: false}` when RSA isn't configured.
#[derive(Serialize, ToSchema)]
pub struct RsaKeyResponse {
    pub enabled: bool,
    /// PEM — present when `enabled`.
    #[serde(rename = "publicKey", skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    #[serde(rename = "keyId", skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
}

/* ============================== meta ================================= */

/// `GET /api/meta` → `{siteName, botUsername, appUrl}`.
#[derive(Serialize, ToSchema)]
pub struct MetaResponse {
    #[serde(rename = "siteName")]
    pub site_name: String,
    #[serde(rename = "botUsername")]
    pub bot_username: Option<String>,
    #[serde(rename = "appUrl")]
    pub app_url: String,
}

/* ============================= account =============================== */

/// `GET /api/me` — the signed-in identity + effective permission keys.
#[derive(Serialize, ToSchema)]
pub struct MeResponse {
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    pub telegram_id: Option<String>,
    /// Sorted permission keys (`*` for the admin role).
    pub permissions: Vec<String>,
    /// HMAC request-signing key — `null` when unconfigured.
    #[serde(rename = "signingKey")]
    pub signing_key: Option<String>,
    /// unix seconds — when the session row was created.
    #[serde(rename = "sessionCreatedAt")]
    pub session_created_at: Option<i64>,
}

/// `GET /api/profile/link` → `{telegram_id, bot_profile}`.
#[derive(Serialize, ToSchema)]
pub struct LinkState {
    pub telegram_id: Option<String>,
    /// `@username` or first name of the linked Telegram account.
    pub bot_profile: Option<String>,
}

/// `POST /api/profile/link` → `{code, bot_username, bot_link}`.
#[derive(Serialize, ToSchema)]
pub struct LinkCreated {
    /// One-time `/link CODE` (8 base64url-safe chars).
    pub code: String,
    pub bot_username: String,
    /// `https://t.me/<bot>`.
    pub bot_link: String,
}

/* ============================= friends =============================== */

/// One `friend_links` row + the computed display `name`.
#[derive(Serialize, ToSchema)]
pub struct FriendDto {
    pub id: i64,
    pub telegram_id: String,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub shared: i64,
    /// `"first last" || username || telegram_id`.
    pub name: String,
}

/// `GET /api/friends` → `{friends: [...]}`.
#[derive(Serialize, ToSchema)]
pub struct FriendsResponse {
    pub friends: Vec<FriendDto>,
}

/// `POST /api/friends` — `{ "action": "invite" }` (only supported action).
#[derive(Serialize, ToSchema)]
pub struct InviteBody {
    pub action: String,
}

/// `POST /api/friends` → `{code, bot_link}`.
#[derive(Serialize, ToSchema)]
pub struct InviteResponse {
    pub code: String,
    /// `https://t.me/<bot>?start=<code>` — 7-day one-time invite.
    pub bot_link: String,
}

/* ============================== albums =============================== */

/// The `AlbumModel` JSON shape (list + detail responses).
#[derive(Serialize, ToSchema)]
pub struct AlbumDto {
    pub id: i64,
    pub slug: String,
    #[serde(rename = "ownerId")]
    pub owner_id: Option<i64>,
    pub title: String,
    /// Public `/a/<token>` link token.
    pub token: String,
    #[serde(rename = "isPublic")]
    pub is_public: bool,
    #[serde(rename = "publicExpiresAt")]
    pub public_expires_at: Option<i64>,
    #[serde(rename = "itemCount")]
    pub item_count: i64,
    #[serde(rename = "coverSlug")]
    pub cover_slug: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub views: i64,
    pub likes: i64,
    #[serde(rename = "botUsername")]
    pub bot_username: Option<String>,
}

/// `GET /api/albums` → `{albums: [...]}`.
#[derive(Serialize, ToSchema)]
pub struct AlbumsResponse {
    pub albums: Vec<AlbumDto>,
}

/// `POST /api/albums` — `{title}`.
#[derive(Serialize, ToSchema)]
pub struct CreateAlbumBody {
    pub title: String,
}

/// `POST /api/albums` → `{slug}`.
#[derive(Serialize, ToSchema)]
pub struct AlbumCreated {
    pub slug: String,
}

/// `PATCH /api/albums/{slug}` — `{isPublic?, title?}`.
#[derive(Serialize, ToSchema)]
pub struct PatchAlbumBody {
    /// Toggle the public link — requires `share:album`. Non-boolean
    /// values are ignored for the write but echoed in the response.
    #[serde(rename = "isPublic", skip_serializing_if = "Option::is_none")]
    pub is_public: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// `PATCH /api/albums/{slug}` → `{ok, is_public, token, expiresAt}`
/// (the TS route's snake/camel mix, verbatim).
#[derive(Serialize, ToSchema)]
pub struct PatchAlbumResponse {
    pub ok: bool,
    /// Echoes the request's `isPublic` verbatim (any JSON type).
    pub is_public: serde_json::Value,
    pub token: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<i64>,
}

/// `POST /api/albums/{slug}/files` multipart form — one `file` part,
/// image or video ≤ 50 MB. Sign-exempt (CSRF only).
#[derive(Serialize, ToSchema)]
pub struct UploadForm {
    /// The file to store (image/* or video/*).
    #[schema(format = "binary")]
    pub file: String,
}

/// `POST /api/albums/{slug}/files` → `{ok, item}`.
#[derive(Serialize, ToSchema)]
pub struct UploadResponse {
    pub ok: bool,
    pub item: UploadedItem,
}

#[derive(Serialize, ToSchema)]
pub struct UploadedItem {
    pub slug: String,
    /// `photo` | `video` | `document`.
    pub kind: String,
    /// True when the file can stream through the Bot API (`available`).
    pub availability: bool,
}

/* ============================== media ================================ */

/// The gallery item shape — `GET /api/media` items.
#[derive(Serialize, ToSchema)]
pub struct MediaItemDto {
    pub slug: String,
    #[serde(rename = "ownerId")]
    pub owner_id: Option<i64>,
    /// `photo` | `video` | `document`.
    pub kind: String,
    pub caption: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<i64>,
    #[serde(rename = "fileSize")]
    pub file_size: Option<i64>,
    /// `available` | `telegram-only` | `dead`.
    pub availability: String,
    #[serde(rename = "sharesCount")]
    pub shares_count: i64,
    /// unix seconds.
    #[serde(rename = "createdAt")]
    pub created_at: i64,
}

/// `GET /api/media` → `{items, nextCursor}` — keyset-paginated.
#[derive(Serialize, ToSchema)]
pub struct MediaListResponse {
    pub items: Vec<MediaItemDto>,
    /// Opaque cursor to echo back as `?cursor=` — `null` on the last page.
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

/// `GET /api/media/summary` → `{count, bytes}`.
#[derive(Serialize, ToSchema)]
pub struct MediaSummary {
    pub count: i64,
    pub bytes: i64,
}

/// One live `/s/<token>` link — `GET /api/media/{slug}/share` items.
#[derive(Serialize, ToSchema)]
pub struct ShareLinkDto {
    pub token: Option<String>,
    /// `APP_URL + /s/<token>`.
    pub url: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<i64>,
    pub views: i64,
    pub likes: i64,
}

#[derive(Serialize, ToSchema)]
pub struct ShareLinksResponse {
    pub links: Vec<ShareLinkDto>,
}

/// `POST /api/media/{slug}/share` — `{mode: "link"}` mints a public
/// `/s/<token>`; `{mode: "telegram", friend_id}` delivers via the bot.
#[derive(Serialize, ToSchema)]
pub struct CreateShareBody {
    /// `link` | `telegram`.
    pub mode: String,
    /// Required for `mode: "telegram"` — a `friend_links` row id.
    pub friend_id: Option<i64>,
}

/// `mode:"link"` response — `{mode, url, token, expiresAt}`.
#[derive(Serialize, ToSchema)]
pub struct LinkShareCreated {
    pub mode: String,
    pub url: String,
    pub token: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: i64,
}

/// `mode:"telegram"` response — `{mode, ok}`.
#[derive(Serialize, ToSchema)]
pub struct TelegramShareSent {
    pub mode: String,
    pub ok: bool,
}

/// `DELETE /api/media/{slug}/share` — `{token}` revokes one link.
#[derive(Serialize, ToSchema)]
pub struct RevokeShareBody {
    pub token: String,
}

/// `POST /api/media/move` — `{slugs, albumId}`; `albumId: null` files
/// back into the vault.
#[derive(Serialize, ToSchema)]
pub struct MoveBody {
    /// ≤ 100 media slugs.
    pub slugs: Vec<String>,
    /// Target album id, or `null` for the vault.
    #[serde(rename = "albumId")]
    pub album_id: Option<i64>,
}

/// `POST /api/media/move` → `{ok, moved, failed, skipped}`.
#[derive(Serialize, ToSchema)]
pub struct MoveResponse {
    pub ok: bool,
    pub moved: i64,
    pub failed: i64,
    pub skipped: i64,
}

/// `PATCH /api/media/{slug}` — `{caption}`; `null`/empty clears it.
#[derive(Serialize, ToSchema)]
pub struct PatchMediaBody {
    pub caption: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PatchMediaResponse {
    pub ok: bool,
    pub caption: Option<String>,
}

/* ============================== public =============================== */

/// `GET /api/public/share/{token}` — the `/s/<token>` page's metadata.
#[derive(Serialize, ToSchema)]
pub struct PublicShareDto {
    pub slug: String,
    pub kind: String,
    pub caption: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<i64>,
    #[serde(rename = "fileSize")]
    pub file_size: Option<i64>,
    pub availability: String,
    #[serde(rename = "ownerName")]
    pub owner_name: Option<String>,
    /// Share creation time — unix seconds.
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<i64>,
    pub likes: i64,
}

/// Album head — present on `GET /api/public/album` only when no cursor
/// is passed (the first-page render).
#[derive(Serialize, ToSchema)]
pub struct PublicAlbumHead {
    pub title: String,
    #[serde(rename = "ownerName")]
    pub owner_name: Option<String>,
    pub likes: i64,
    #[serde(rename = "publicExpiresAt")]
    pub public_expires_at: Option<i64>,
    #[serde(rename = "itemCount")]
    pub item_count: i64,
    #[serde(rename = "coverSlug")]
    pub cover_slug: Option<String>,
}

/// The trimmed public-album item shape.
#[derive(Serialize, ToSchema)]
pub struct PublicMediaItem {
    pub slug: String,
    pub kind: String,
    pub caption: Option<String>,
    pub availability: String,
}

/// `GET /api/public/album` → `{album, items, nextCursor}`.
#[derive(Serialize, ToSchema)]
pub struct PublicAlbumResponse {
    /// `null` on paginated (cursor-bearing) calls.
    pub album: Option<PublicAlbumHead>,
    pub items: Vec<PublicMediaItem>,
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

/// `POST /api/public/*/like` → `{likes, liked: true}`.
#[derive(Serialize, ToSchema)]
pub struct LikeResponse {
    pub likes: i64,
    pub liked: bool,
}

/* ============================== admin ================================ */

/// `GET /api/admin/access` → `{telegram_ids}` (sorted).
#[derive(Serialize, ToSchema)]
pub struct AdminAccessList {
    pub telegram_ids: Vec<String>,
}

/// `POST|DELETE /api/admin/access` → `{ok, telegram_ids}`.
#[derive(Serialize, ToSchema)]
pub struct AdminAccessMut {
    pub ok: bool,
    pub telegram_ids: Vec<String>,
}

/// One `AuditRow` — `GET /api/admin/activity` items.
#[derive(Serialize, ToSchema)]
pub struct AuditEntryDto {
    /// Synthetic `at * 1000 + index` — list key only.
    pub id: i64,
    /// unix seconds (alias of `created_at`).
    pub at: i64,
    pub created_at: i64,
    pub actor_user_id: Option<i64>,
    pub actor_name: Option<String>,
    /// e.g. `media.upload`, `role.permissions.update`.
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub detail: Option<String>,
}

/// `GET /api/admin/activity` → `{activity: [...]}` (newest 100).
#[derive(Serialize, ToSchema)]
pub struct ActivityResponse {
    pub activity: Vec<AuditEntryDto>,
}

/// Permission keys grouped by resource (catalog order).
#[derive(Serialize, ToSchema)]
pub struct PermissionGroup {
    /// The part before `:` — `media`, `album`, `user`, …
    pub resource: String,
    pub keys: Vec<String>,
}

/// `GET /api/admin/permissions` → `{permissions: [...]}`.
#[derive(Serialize, ToSchema)]
pub struct PermissionsResponse {
    pub permissions: Vec<PermissionGroup>,
}

/// One role in `GET /api/admin/roles`.
#[derive(Serialize, ToSchema)]
pub struct RoleDto {
    pub id: i64,
    pub name: String,
    /// `1` for built-in roles (admin/viewer/…) — can't be renamed/deleted.
    pub builtin: i64,
    pub permissions: Vec<String>,
    #[serde(rename = "memberCount")]
    pub member_count: i64,
}

/// `GET /api/admin/roles` → `{roles: [...]}`.
#[derive(Serialize, ToSchema)]
pub struct RolesResponse {
    pub roles: Vec<RoleDto>,
}

/// `PATCH /api/admin/roles/{id}` — `{name}` renames (custom roles
/// only), `{permissions}` replaces the permission set.
#[derive(Serialize, ToSchema)]
pub struct PatchRoleBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Permission keys — `*` is reserved for the built-in admin role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
}

/// `POST /api/admin/roles` → `{ok, id, name}`.
#[derive(Serialize, ToSchema)]
pub struct RoleCreated {
    pub ok: bool,
    pub id: i64,
    pub name: String,
}

/// `GET /api/admin/settings` → `{siteName}`.
#[derive(Serialize, ToSchema)]
pub struct SettingsResponse {
    #[serde(rename = "siteName")]
    pub site_name: String,
}

/// `PATCH /api/admin/settings` → `{ok, siteName}`.
#[derive(Serialize, ToSchema)]
pub struct SettingsPatched {
    pub ok: bool,
    #[serde(rename = "siteName")]
    pub site_name: String,
}

/// One user in `GET /api/admin/users`.
#[derive(Serialize, ToSchema)]
pub struct AdminUserDto {
    pub id: i64,
    pub email: Option<String>,
    pub name: String,
    pub telegram_id: Option<String>,
    /// unix seconds — non-null while suspended.
    pub disabled_at: Option<i64>,
    pub created_at: i64,
    /// Role names assigned to this user.
    pub roles: Vec<String>,
}

/// `{id, name, builtin}` role ref (no permission expansion).
#[derive(Serialize, ToSchema)]
pub struct RoleRefDto {
    pub id: i64,
    pub name: String,
    pub builtin: i64,
}

/// `GET /api/admin/users` → `{users, roles}`.
#[derive(Serialize, ToSchema)]
pub struct UsersResponse {
    pub users: Vec<AdminUserDto>,
    pub roles: Vec<RoleRefDto>,
}

/// `PATCH /api/admin/users/{id}` — profile fields (`name`/`email`/
/// `password`) OR `{disabled: bool}` suspend/unsuspend.
#[derive(Serialize, ToSchema)]
pub struct PatchUserBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Non-empty resets the password and kills the user's sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

/// `POST /api/admin/users` → `{ok, id}`.
#[derive(Serialize, ToSchema)]
pub struct UserCreated {
    pub ok: bool,
    pub id: i64,
}

/// `GET /api/admin/cache` → `{ttlDays, last}`.
#[derive(Serialize, ToSchema)]
pub struct CacheState {
    #[serde(rename = "ttlDays")]
    pub ttl_days: i64,
    /// Stats from the last sweep — `null` before the first run.
    pub last: Option<admin::cache::CacheCleanupStats>,
}

/// `PATCH /api/admin/cache` → `{ok, ttlDays}`.
#[derive(Serialize, ToSchema)]
pub struct PatchCacheResponse {
    pub ok: bool,
    #[serde(rename = "ttlDays")]
    pub ttl_days: i64,
}

/// `POST /api/admin/cache` → `{ok, stats}`.
#[derive(Serialize, ToSchema)]
pub struct RunCacheResponse {
    pub ok: bool,
    pub stats: admin::cache::CacheCleanupStats,
}

/// `{count, bytes}` — media/vault totals.
#[derive(Serialize, ToSchema)]
pub struct CountBytes {
    pub count: i64,
    pub bytes: i64,
}

#[derive(Serialize, ToSchema)]
pub struct AlbumCounts {
    pub count: i64,
    #[serde(rename = "publicCount")]
    pub public_count: i64,
}

#[derive(Serialize, ToSchema)]
pub struct ShareCounts {
    pub count: i64,
    #[serde(rename = "activeLinks")]
    pub active_links: i64,
}

#[derive(Serialize, ToSchema)]
pub struct UserCounts {
    pub count: i64,
    pub active: i64,
}

/// `topOwners[]` — `{name, n, bytes}` per media owner, top 5.
#[derive(Serialize, ToSchema)]
pub struct TopOwner {
    pub name: String,
    pub n: i64,
    pub bytes: i64,
}

/// `GET /api/admin/vault` — the Vault tab's DB index stats.
#[derive(Serialize, ToSchema)]
pub struct VaultStats {
    pub media: CountBytes,
    /// `{<kind>: n}` — photo/video/document counts.
    #[serde(rename = "byKind")]
    pub by_kind: HashMap<String, i64>,
    /// `{<availability>: n}` — available/telegram-only/dead.
    #[serde(rename = "byAvailability")]
    pub by_availability: HashMap<String, i64>,
    pub albums: AlbumCounts,
    pub shares: ShareCounts,
    pub users: UserCounts,
    #[serde(rename = "topOwners")]
    pub top_owners: Vec<TopOwner>,
}

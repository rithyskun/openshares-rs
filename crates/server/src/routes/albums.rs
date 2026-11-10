//! Session-gated album endpoints — ports of:
//! - `src/app/api/albums/route.ts`            → `POST /api/albums` (create).
//!   `GET /api/albums` is the API form of the gallery page's album query
//!   (`src/app/gallery/page.tsx`): `list_all_with_stats` + lazy revoke of
//!   expired public links + the `AlbumModel` field shape.
//! - `src/app/api/albums/[slug]/route.ts`     → `PATCH`/`DELETE
//!   `/api/albums/{slug}`. `GET` is the API form of
//!   `src/app/album/[slug]/page.tsx` — 404 for missing AND non-owned
//!   albums (a 403 would confirm an album exists at that slug).
//! - `src/app/api/albums/[slug]/files/route.ts` → `POST
//!   `/api/albums/{slug}/files` (multipart `file=` upload → Telegram
//!   storage chat via `sendPhoto`/`sendVideo`/`sendDocument` → media row).
//!
//! Permission checks mirror the TS exactly: `apiGuard` failures are the
//! `forbidden()` `"Forbidden."` 403 (create/upload), explicit `can()`
//! failures are `"Not allowed."`; GET handlers 404 on non-ownership like
//! the pages. Mutating JSON bodies go through `crate::body::json_body`
//! (HMAC signing); `/albums/{slug}/files` is sign-EXEMPT (multipart
//! boundary unhashable) so its body is parsed raw — CSRF-only, same as TS.
//!
//! axum's `multipart` feature is off in this workspace, so the multipart
//! body is parsed in-file: boundary split + per-part headers, the `file`
//! field requiring a `filename` (matching `form.get("file") instanceof
//! File` in the TS route).

use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use openshares_core::audit::audit;
use openshares_core::bot::{BotContext, TgMessage, extract_media_from_message, get_bot_username};
use openshares_core::crypto::security::{Limiter, rate_limited};
use openshares_core::db::repos::media::{MediaListQuery, MediaScope};
use openshares_core::db::repos::{albums, media};
use openshares_core::db::{PUBLIC_LINK_TTL, now};
use openshares_core::settings::stored_bot_username;
use openshares_core::telegram::{TgMessageOut, file_form};
use openshares_core::util::{new_token, random_slug};

use crate::body::{json_body, raw_body};
use crate::error::{ApiError, Result};
use crate::extract::AuthUser;
use crate::state::AppState;

/// `MAX_BYTES` — Bot API upload limit per file (50 MB).
const MAX_BYTES: usize = 50 * 1024 * 1024;
/// `PHOTO_UPLOAD_LIMIT` — sendPhoto's own 10 MB cap; bigger images go
/// out as documents.
const PHOTO_UPLOAD_LIMIT: usize = 10 * 1024 * 1024;
/// Multipart framing allowance over the file cap (TS `+ 8192`).
const MULTIPART_OVERHEAD: usize = 8192;

/// Route table — merged by `routes::api_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/albums", get(list_albums).post(create_album))
        .route(
            "/albums/{slug}",
            get(get_album).patch(patch_album).delete(delete_album),
        )
        .route("/albums/{slug}/files", post(upload_album_file))
}

/* ------------------------- shared helpers ------------------------- */

/// `tooManyRequests()` — 429 `{error}` + `Retry-After: 60`, verbatim.
pub(crate) fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "60")],
        Json(json!({ "error": "Too many requests — slow down and retry shortly." })),
    )
        .into_response()
}

/// `getBotUsername().catch(() => null)` — the settings-stored username
/// wins; `getMe` runs only when a `TgClient` exists (`BOT_TOKEN` set).
pub(crate) async fn bot_username(state: &AppState) -> Option<String> {
    match state.tg.clone() {
        Some(tg) => {
            get_bot_username(&BotContext {
                db: state.db.clone(),
                tg,
                app_url: state.config.app_url.clone(),
                cache: state.cache.clone(),
            })
            .await
        }
        None => stored_bot_username(&state.db).await.ok().flatten(),
    }
}

/// Everything [`album_json`] needs — filled from `AlbumListRow` (list)
/// or `Album` + computed stats (detail).
struct AlbumView<'a> {
    id: i64,
    slug: &'a str,
    owner_user_id: Option<i64>,
    title: &'a str,
    token: &'a str,
    is_public: bool,
    public_expires_at: Option<i64>,
    item_count: i64,
    cover_slug: Option<&'a str>,
    created_at: i64,
    views: i64,
    likes: i64,
    bot: Option<&'a str>,
}

/// The `AlbumModel` JSON shape (AlbumCard.tsx) — same field names the
/// gallery/album pages hand to the client.
fn album_json(v: &AlbumView<'_>) -> Value {
    json!({
        "id": v.id,
        "slug": v.slug,
        "ownerId": v.owner_user_id,
        "title": v.title,
        "token": v.token,
        "isPublic": v.is_public,
        "publicExpiresAt": v.public_expires_at,
        "itemCount": v.item_count,
        "coverSlug": v.cover_slug,
        "createdAt": v.created_at,
        "views": v.views,
        "likes": v.likes,
        "botUsername": v.bot,
    })
}

/// The gallery/album-page lazy revoke: a public link past its expiry is
/// flipped back to private inline so the owner sees sharing stopped.
async fn revoke_if_expired(
    state: &AppState,
    id: i64,
    is_public: &mut i64,
    expires: Option<i64>,
) -> Result<()> {
    if *is_public == 1 && expires.is_some_and(|e| e <= now()) {
        albums::revoke_public(&state.db, id).await?;
        *is_public = 0;
    }
    Ok(())
}

/* ------------------------- GET/POST /albums ------------------------- */

/// `GET /api/albums` — `media:view` gated (the gallery page's
/// `requirePerm`), all albums newest-first with item counts and covers —
/// the page lists **all** albums regardless of owner (shared vault).
async fn list_albums(State(state): State<AppState>, user: AuthUser) -> Result<Response> {
    if !user.0.can("media:view", None) {
        return Err(ApiError::forbidden("Not allowed."));
    }
    // The album rows and the bot-username setting are independent remote
    // round trips — overlap them.
    let (rows, bot) = tokio::join!(albums::list_all_with_stats(&state.db), bot_username(&state),);
    let rows = rows?;

    let mut out = Vec::with_capacity(rows.len());
    for mut a in rows {
        revoke_if_expired(&state, a.id, &mut a.is_public, a.public_expires_at).await?;
        out.push(album_json(&AlbumView {
            id: a.id,
            slug: &a.slug,
            owner_user_id: a.owner_user_id,
            title: &a.title,
            token: &a.token,
            is_public: a.is_public != 0,
            public_expires_at: a.public_expires_at,
            item_count: a.item_count,
            cover_slug: a.cover_slug.as_deref(),
            created_at: a.created_at,
            views: a.views,
            likes: a.likes,
            bot: bot.as_deref(),
        }));
    }
    Ok(Json(json!({ "albums": out })).into_response())
}

/// `POST /api/albums { title }` — `apiGuard("album:create")` → 403
/// `"Forbidden."`; empty title → `"Give the album a title."` 400.
/// Response `{slug}`; audit `album.create` (detail = title).
async fn create_album(
    State(state): State<AppState>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    let uid = user.0.id;
    if !user.0.can("album:create", None) {
        return Err(ApiError::forbidden("Forbidden."));
    }
    let body: Map<String, Value> = json_body(req, Some(&user.0.sid)).await?;
    let title: String = match body.get("title") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.trim().chars().take(200).collect(),
        // `(5).trim()` → TypeError → 500 in the TS route.
        Some(_) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error.",
            ));
        }
    };
    if title.is_empty() {
        return Err(ApiError::bad_request("Give the album a title."));
    }

    let slug = random_slug();
    let token = new_token(16);
    albums::create(
        &state.db,
        &albums::NewAlbum {
            slug: &slug,
            owner_user_id: uid,
            title: &title,
            token: &token,
        },
    )
    .await?;
    audit(
        &state.cache,
        Some(uid),
        "album.create",
        Some("album"),
        Some(&slug),
        Some(&title),
    )
    .await;
    Ok(Json(json!({ "slug": slug })).into_response())
}

/* --------------------- /albums/{slug} --------------------- */

/// `GET /api/albums/{slug}` — the album page as JSON: `media:view` for
/// the caller, then 404 for missing or non-owned albums without
/// `album:edit:any` (existence hiding). `itemCount` = live count,
/// `coverSlug` = first item's slug (any state, like the page).
async fn get_album(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    user: AuthUser,
) -> Result<Response> {
    let uid = user.0.id;
    if !user.0.can("media:view", None) {
        return Err(ApiError::forbidden("Not allowed."));
    }
    let mut album = albums::get_by_slug(&state.db, &slug)
        .await?
        .ok_or_else(|| ApiError::not_found("Not found."))?;
    if album.owner_user_id != Some(uid) && !user.0.can("album:edit:any", None) {
        return Err(ApiError::not_found("Not found."));
    }
    revoke_if_expired(
        &state,
        album.id,
        &mut album.is_public,
        album.public_expires_at,
    )
    .await?;

    let item_count = media::count_in_album(&state.db, album.id).await?;
    // `items[0]?.slug` on the album page — first row, missing or not.
    let cover_slug = media::list(
        &state.db,
        &MediaListQuery {
            scope: MediaScope::Album(album.id),
            limit: 1,
            ..Default::default()
        },
    )
    .await?
    .items
    .into_iter()
    .next()
    .map(|m| m.slug);
    let bot = bot_username(&state).await;

    Ok(Json(album_json(&AlbumView {
        id: album.id,
        slug: &album.slug,
        owner_user_id: album.owner_user_id,
        title: &album.title,
        token: &album.token,
        is_public: album.is_public != 0,
        public_expires_at: album.public_expires_at,
        item_count,
        cover_slug: cover_slug.as_deref(),
        created_at: album.created_at,
        views: album.views,
        likes: album.likes,
        bot: bot.as_deref(),
    }))
    .into_response())
}

/// `PATCH /api/albums/{slug} { isPublic?, title? }` — `album:edit`
/// scoped to the album owner; passing `isPublic` additionally requires
/// `share:album`. Audits `album.share`/`album.unshare` (old title) and
/// `album.rename` (new title). Response `{ok, is_public, token,
/// expiresAt}` — the TS route's snake/camel mix, kept verbatim.
async fn patch_album(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    let uid = user.0.id;
    let album = albums::get_by_slug(&state.db, &slug)
        .await?
        .ok_or_else(|| ApiError::not_found("Not found."))?;
    if !user.0.can("album:edit", album.owner_user_id) {
        return Err(ApiError::forbidden("Not allowed."));
    }
    let body: Map<String, Value> = json_body(req, Some(&user.0.sid)).await?;

    let mut expires_at = album.public_expires_at;
    // `body.isPublic ?? !!album.is_public` — a non-null value is echoed
    // verbatim in the response (even a non-bool), else the flag.
    let responded_is_public: Value = match body.get("isPublic") {
        Some(Value::Null) | None => json!(album.is_public != 0),
        Some(v) => v.clone(),
    };
    // `typeof body.isPublic === "boolean"` — non-bool values are ignored.
    if let Some(Value::Bool(public)) = body.get("isPublic") {
        if !user.0.can("share:album", None) {
            return Err(ApiError::forbidden("Not allowed."));
        }
        expires_at = if *public {
            Some(now() + PUBLIC_LINK_TTL)
        } else {
            None
        };
        albums::set_public(&state.db, album.id, *public, expires_at).await?;
        let action = if *public {
            "album.share"
        } else {
            "album.unshare"
        };
        audit(
            &state.cache,
            Some(uid),
            action,
            Some("album"),
            Some(&slug),
            Some(&album.title),
        )
        .await;
    }
    // `typeof body.title === "string"` — empty/same titles are no-ops.
    if let Some(Value::String(title)) = body.get("title") {
        let title: String = title.trim().chars().take(200).collect();
        if !title.is_empty() && title != album.title {
            albums::rename(&state.db, album.id, &title).await?;
            audit(
                &state.cache,
                Some(uid),
                "album.rename",
                Some("album"),
                Some(&slug),
                Some(&title),
            )
            .await;
        }
    }
    Ok(Json(json!({
        "ok": true,
        "is_public": responded_is_public,
        "token": album.token,
        "expiresAt": expires_at,
    }))
    .into_response())
}

/// `DELETE /api/albums/{slug}` — signature verified BEFORE the album
/// lookup (the TS order), `album:delete` scoped to the owner; media
/// rows survive via `ON DELETE SET NULL` → back to the vault. Audit:
/// `album.delete` (detail = title).
async fn delete_album(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    let uid = user.0.id;
    // `verifySignedBody` first — the TS route checks the signature
    // before loading the album.
    raw_body(req, Some(&user.0.sid)).await?;
    let album = albums::get_by_slug(&state.db, &slug)
        .await?
        .ok_or_else(|| ApiError::not_found("Not found."))?;
    if !user.0.can("album:delete", album.owner_user_id) {
        return Err(ApiError::forbidden("Not allowed."));
    }
    albums::delete_by_id(&state.db, album.id).await?;
    audit(
        &state.cache,
        Some(uid),
        "album.delete",
        Some("album"),
        Some(&slug),
        Some(&album.title),
    )
    .await;
    Ok(Json(json!({ "ok": true })).into_response())
}

/* --------------------- /albums/{slug}/files --------------------- */

/// `env("STORAGE_CHANNEL_ID")` — trimmed-or-undefined, read lazily like
/// the TS `env()` helper so rotating it needs no restart.
fn storage_channel_id() -> Option<String> {
    std::env::var("STORAGE_CHANNEL_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `name.replace(/\.[^.]+$/, "").slice(0, 1000)` — strips the LAST
/// extension (a trailing dot with nothing after it isn't one).
fn caption_from_name(name: &str) -> String {
    let stem = name
        .rfind('.')
        .filter(|&i| i + 1 < name.len())
        .map(|i| &name[..i])
        .unwrap_or(name);
    stem.chars().take(1000).collect()
}

/// `POST /api/albums/{slug}/files` — multipart `file=<one file>`
/// upload: `apiGuard("media:upload")` + `API_LIMITER upload:{uid}` +
/// declared-`content-length` cap (413) before the album/`album:edit`
/// checks, then Telegram upload into the storage chat and a media row.
/// Sign-exempt (multipart) — CSRF-only like the TS route.
async fn upload_album_file(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    let uid = user.0.id;
    if !user.0.can("media:upload", None) {
        return Err(ApiError::forbidden("Forbidden."));
    }
    if rate_limited(&state.db, Limiter::Api, &format!("upload:{uid}")).await {
        return Ok(too_many_requests());
    }

    // Reject oversize bodies before buffering (multipart adds ~1 KB).
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if declared > (MAX_BYTES + MULTIPART_OVERHEAD) as u64 {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Telegram accepts files up to 50 MB.",
        ));
    }

    let album = albums::get_by_slug(&state.db, &slug)
        .await?
        .ok_or_else(|| ApiError::not_found("Album not found."))?;
    if !user.0.can("album:edit", album.owner_user_id) {
        return Err(ApiError::forbidden("Not allowed."));
    }

    let Some(storage_chat) = storage_channel_id().or_else(|| user.0.telegram_id.clone()) else {
        return Err(ApiError::bad_request(
            "No storage chat available. Set STORAGE_CHANNEL_ID in .env (or connect your Telegram account), then retry.",
        ));
    };

    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let Some(boundary) = multipart_boundary(&content_type) else {
        return Err(ApiError::bad_request("Could not read the upload."));
    };
    let body = axum::body::to_bytes(req.into_body(), MAX_BYTES + MULTIPART_OVERHEAD)
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Telegram accepts files up to 50 MB.",
            )
        })?;

    let parts = parse_multipart(&body, &boundary);
    let Some(file) = parts
        .iter()
        .find(|p| p.name.as_deref() == Some("file") && p.filename.is_some())
    else {
        return Err(ApiError::bad_request(
            "Send multipart form with a `file` field.",
        ));
    };
    if file.data.is_empty() {
        return Err(ApiError::bad_request("Empty file."));
    }
    if file.data.len() > MAX_BYTES {
        return Err(ApiError::bad_request("Telegram accepts files up to 50 MB."));
    }

    let mime = file
        .content_type
        .clone()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let is_image = mime.starts_with("image/");
    let is_video = mime.starts_with("video/");
    if !is_image && !is_video {
        return Err(ApiError::bad_request(
            "Only images and videos can be stored.",
        ));
    }

    let name = file
        .filename
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| if is_image { "upload.jpg" } else { "upload.mp4" }.to_string());
    let caption = caption_from_name(&name);
    let data = file.data;
    let size = data.len();

    // sendPhoto ≤10 MB images, sendVideo ≤50 MB videos, else sendDocument.
    let (method, field, label, extra) = if is_image && size <= PHOTO_UPLOAD_LIMIT {
        ("sendPhoto", "photo", "image", json!({ "caption": caption }))
    } else if is_video && size <= MAX_BYTES {
        (
            "sendVideo",
            "video",
            "video",
            json!({ "caption": caption, "supports_streaming": "true" }),
        )
    } else {
        (
            "sendDocument",
            "document",
            "file",
            json!({ "caption": caption }),
        )
    };
    let message: TgMessageOut = state
        .require_tg()?
        .upload(method, || {
            file_form(&storage_chat, field, data, &mime, &name, Some(&extra))
        })
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("Telegram rejected the {label}: {e}"),
            )
        })?;

    let extracted = extract_media_from_message(&TgMessage {
        from: None,
        chat: message.chat,
        text: None,
        caption: message.caption,
        photo: message.photo,
        video: message.video,
        document: message.document,
        media_group_id: None,
    })
    .ok_or_else(|| ApiError::new(StatusCode::BAD_GATEWAY, "Telegram response was unexpected."))?;

    let media_slug = random_slug();
    media::insert(
        &state.db,
        &media::NewMedia {
            slug: &media_slug,
            owner_user_id: Some(uid),
            telegram_id: "upload", // provenance marker
            album_id: Some(album.id),
            kind: extracted.kind,
            file_id: extracted.file_id.as_str(),
            file_unique_id: extracted.file_unique_id.as_deref(),
            thumb_file_id: extracted.thumb_file_id.as_deref(),
            file_size: extracted.file_size.and_then(|v| i64::try_from(v).ok()),
            width: extracted.width,
            height: extracted.height,
            duration: extracted.duration,
            mime: extracted.mime.as_deref(),
            caption: (!caption.is_empty()).then_some(caption.as_str()),
            availability: if extracted.streamable {
                "available"
            } else {
                "telegram-only"
            },
            media_group_id: None,
        },
    )
    .await?;

    let detail = format!("{name} ({size} B) → {slug}");
    audit(
        &state.cache,
        Some(uid),
        "media.upload",
        Some("media"),
        Some(&media_slug),
        Some(&detail),
    )
    .await;
    Ok(Json(json!({
        "ok": true,
        "item": {
            "slug": media_slug,
            "kind": extracted.kind,
            "availability": extracted.streamable,
        },
    }))
    .into_response())
}

/* --------------------- minimal multipart parser --------------------- */

/// One parsed multipart part — headers lowercased into `name`/
/// `filename`/`content_type`, `data` borrowing the request body.
#[derive(Debug)]
struct MultipartPart<'a> {
    name: Option<String>,
    filename: Option<String>,
    content_type: Option<String>,
    data: &'a [u8],
}

/// `boundary=` parameter of a `multipart/form-data` Content-Type —
/// `None` for any other media type (the TS `formData()` failure → 400).
fn multipart_boundary(content_type: &str) -> Option<String> {
    let (mime, params) = content_type.split_once(';').unwrap_or((content_type, ""));
    if !mime.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for param in params.split(';') {
        let Some((k, v)) = param.trim().split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("boundary") {
            let v = v.trim().trim_matches('"');
            return (!v.is_empty()).then(|| v.to_string());
        }
    }
    None
}

/// Byte-slice search (no memchr dep) — index of `needle` at/after `from`.
fn find_subslice(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() || haystack.len() - from < needle.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

/// Split a multipart body on `--boundary`. Delimiters: `--boundary\r\n`
/// (first), `\r\n--boundary` (part end), `--boundary--`/`--boundary--\r\n`
/// (closing). Marker-looking bytes inside content that aren't real
/// delimiters are skipped — the sender is required to choose a boundary
/// absent from the payload.
fn parse_multipart<'a>(body: &'a [u8], boundary: &str) -> Vec<MultipartPart<'a>> {
    let marker = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    let mut cursor = 0;
    while let Some(pos) = find_subslice(body, &marker, cursor) {
        let after = pos + marker.len();
        match body.get(after..after + 2) {
            Some(b"--") => break, // closing delimiter — epilogue ignored
            Some(b"\r\n") => {}   // part delimiter
            _ => {
                // Not a delimiter (marker text inside content/preamble).
                cursor = pos + 1;
                continue;
            }
        }
        let part_start = after + 2;
        // Part content runs to the next `\r\n--boundary`.
        let mut scan = part_start;
        let (end, next) = loop {
            match find_subslice(body, &marker, scan) {
                Some(p) if p >= 2 && &body[p - 2..p] == b"\r\n" => break (p - 2, p),
                Some(p) => scan = p + 1,
                None => return parts,
            }
        };
        cursor = next;
        parts.push(parse_part(&body[part_start..end]));
    }
    parts
}

/// One part: `<headers>\r\n\r\n<body>`. Header lines are `Name: value`
/// (case-insensitive); `Content-Disposition` params are parsed
/// quote-aware (a filename may contain `;`).
fn parse_part<'a>(raw: &'a [u8]) -> MultipartPart<'a> {
    let (head, data) = match find_subslice(raw, b"\r\n\r\n", 0) {
        Some(i) => (&raw[..i], &raw[i + 4..]),
        None => (raw, &raw[raw.len()..]),
    };
    let head = String::from_utf8_lossy(head);
    let mut name = None;
    let mut filename = None;
    let mut content_type = None;
    for line in head.split("\r\n") {
        let Some((hname, hval)) = line.split_once(':') else {
            continue;
        };
        if hname.trim().eq_ignore_ascii_case("content-disposition") {
            for (k, v) in header_params(hval) {
                match k.as_str() {
                    "name" => name = Some(v),
                    "filename" => filename = Some(v),
                    _ => {}
                }
            }
        } else if hname.trim().eq_ignore_ascii_case("content-type") {
            content_type = Some(hval.trim().to_string());
        }
    }
    MultipartPart {
        name,
        filename,
        content_type,
        data,
    }
}

/// `k=v; k2="v;2"` — `;`-separated params with `"` quoting honored;
/// values unquoted and `\"`-unescaped.
fn header_params(value: &str) -> Vec<(String, String)> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in value.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                cur.push(c);
            }
            ';' if !in_quotes => segs.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    segs.push(cur);
    segs.into_iter()
        .filter_map(|seg| {
            let (k, v) = seg.trim().split_once('=')?;
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v);
            Some((k.trim().to_ascii_lowercase(), v.replace("\\\"", "\"")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_extraction() {
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=abc123"),
            Some("abc123".to_string())
        );
        assert_eq!(
            multipart_boundary("multipart/form-data; charset=x; boundary=\"q-1\""),
            Some("q-1".to_string())
        );
        assert!(multipart_boundary("application/json").is_none());
        assert!(multipart_boundary("multipart/form-data").is_none());
        assert!(multipart_boundary("multipart/form-data; boundary=").is_none());
    }

    fn sample_body() -> (Vec<u8>, String) {
        let boundary = "----xyzBOUNDARY";
        let body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nhello\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a b.jpg\"\r\n\
             Content-Type: image/jpeg\r\n\r\nJPEGDATA\r\n--{b}--\r\n",
            b = boundary
        );
        (body.into_bytes(), boundary.to_string())
    }

    #[test]
    fn multipart_parses_file_field() {
        let (body, boundary) = sample_body();
        let parts = parse_multipart(&body, &boundary);
        assert_eq!(parts.len(), 2);
        let file = &parts[1];
        assert_eq!(file.name.as_deref(), Some("file"));
        assert_eq!(file.filename.as_deref(), Some("a b.jpg"));
        assert_eq!(file.content_type.as_deref(), Some("image/jpeg"));
        assert_eq!(file.data, b"JPEGDATA");
        assert_eq!(parts[0].name.as_deref(), Some("note"));
        assert_eq!(parts[0].data, b"hello");
    }

    #[test]
    fn multipart_ignores_marker_like_content() {
        let boundary = "XYZ";
        // payload embeds `--XYZ` without the CRLF prefix → not a delimiter
        let payload = b"AA--XYZBB";
        let mut body = Vec::new();
        body.extend_from_slice(
            b"--XYZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"f.bin\"\r\n\r\n",
        );
        body.extend_from_slice(payload);
        body.extend_from_slice(b"\r\n--XYZ--\r\n");
        let parts = parse_multipart(&body, boundary);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].data, payload);
    }

    #[test]
    fn header_params_quote_aware() {
        let p = header_params("form-data; name=\"file\"; filename=\"a;b.jpg\"");
        assert_eq!(
            p,
            vec![
                ("name".to_string(), "file".to_string()),
                ("filename".to_string(), "a;b.jpg".to_string()),
            ]
        );
    }

    #[test]
    fn caption_from_name_rules() {
        assert_eq!(caption_from_name("a.jpg"), "a");
        assert_eq!(caption_from_name("a.b.jpg"), "a.b");
        assert_eq!(caption_from_name("noext"), "noext");
        assert_eq!(caption_from_name("a."), "a.");
        assert_eq!(caption_from_name(".hidden"), "");
        assert_eq!(caption_from_name(&"x".repeat(1500)).len(), 1000);
    }
}

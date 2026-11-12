//! Telegram Bot API client — port of `src/lib/telegram.ts`.
//!
//! Semantics mirrored from the TS version:
//! - `call`/`upload` never panic on API errors — they return
//!   `Result<T, String>` where `Err` carries the Bot API `description`
//!   or a transport error (the `{ok:false,error}` shape).
//! - Transport failures are retried exactly once; a received response
//!   is NEVER replayed (nothing can be double-sent).
//! - File downloads (`open_telegram_file`, `ensure_cached`) stream from
//!   `api.telegram.org/file/...` with Range forwarding and a 120s timeout.
//! - Bot API download limit ~20 MB marks larger items non-streamable
//!   (`is_streamable_size`).

use std::time::Duration;

use reqwest::Client;
use reqwest::multipart::{Form, Part};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::media::{CacheBackend, cache_key};

/// `tgCall` JSON timeout — mirrors `AbortSignal.timeout(60_000)`.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// File download timeout — mirrors the 120s timeout in telegram.ts.
const FILE_TIMEOUT: Duration = Duration::from_secs(120);
/// Multipart upload timeout — mirrors `AbortSignal.timeout(180_000)`.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// Bot API file download ceiling.
const MAX_STREAMABLE_BYTES: u64 = 20 * 1024 * 1024;

/// `TgResponse<T>` equivalent — `Ok(result)` | `Err(description-or-transport)`.
pub type TgResult<T> = Result<T, String>;

// ── Bot API types (subset of telegram.ts) ────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct TgUser {
    pub id: i64,
    pub is_bot: Option<bool>,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub width: i64,
    pub height: i64,
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TgVideo {
    pub file_id: String,
    pub file_unique_id: Option<String>,
    pub width: i64,
    pub height: i64,
    pub duration: i64,
    pub mime_type: Option<String>,
    pub file_size: Option<u64>,
    pub thumbnail: Option<PhotoSize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TgDocument {
    pub file_id: String,
    pub file_unique_id: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<u64>,
    pub thumbnail: Option<PhotoSize>,
}

/// `send*` methods return a Message — mirrors `TgMessageOut`.
#[derive(Debug, Clone, Deserialize)]
pub struct TgMessageOut {
    pub message_id: i64,
    pub chat: TgChat,
    pub photo: Option<Vec<PhotoSize>>,
    pub video: Option<TgVideo>,
    pub document: Option<TgDocument>,
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TgChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: Option<String>,
    pub title: Option<String>,
}

/// One entry in a `sendMediaGroup` payload — `MediaGroupItem`.
#[derive(Debug, Clone, Serialize)]
pub struct MediaGroupItem {
    #[serde(rename = "type")]
    pub kind: MediaKind,
    pub media: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Photo,
    Video,
}

/// `getFile` result fields we use.
#[derive(Debug, Deserialize)]
struct FileResult {
    file_path: Option<String>,
    file_size: Option<u64>,
}

/// `getWebhookInfo` result.
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookInfo {
    pub url: String,
    pub pending_update_count: i64,
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

// ── MIME table (MIME_BY_EXT in telegram.ts) ──────────────────────────

fn mime_for_ext(ext: &str) -> Option<&'static str> {
    // ext includes the leading dot, already lowercased by caller
    match ext {
        ".jpg" | ".jpeg" => Some("image/jpeg"),
        ".png" => Some("image/png"),
        ".webp" => Some("image/webp"),
        ".gif" => Some("image/gif"),
        ".heic" => Some("image/heic"),
        ".mp4" => Some("video/mp4"),
        ".mov" => Some("video/quicktime"),
        ".webm" => Some("video/webm"),
        ".mkv" => Some("video/x-matroska"),
        ".avi" => Some("video/x-msvideo"),
        _ => None,
    }
}

/// `path.extname` equivalent — returns ".ext" (original case) or "".
fn ext_of(file_path: &str) -> String {
    let name = file_path.rsplit('/').next().unwrap_or(file_path);
    match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => format!(".{ext}"),
        _ => String::new(),
    }
}

// ── Client ───────────────────────────────────────────────────────────

/// Bot API client. `Clone` is cheap — shares one `reqwest::Client`
/// (connection pool) across the app.
#[derive(Clone)]
pub struct TgClient {
    client: Client,
    token: String,
}

impl TgClient {
    pub fn new(token: impl Into<String>) -> Self {
        // Default 60s ceiling mirrors the tgCall timeout; file/upload
        // calls override per-request (120s/180s).
        let client = Client::builder()
            .timeout(CALL_TIMEOUT)
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            token: token.into(),
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
    }

    fn file_url(&self, file_path: &str) -> String {
        format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.token, file_path
        )
    }

    /// `reqwest::Error` Display embeds the request URL — and every Bot
    /// API URL carries the token — so strip it before error strings
    /// reach logs or route responses (TS `fetch` errors never included
    /// the URL).
    fn redact(&self, msg: String) -> String {
        msg.replace(&self.token, "[redacted]")
    }

    /// One JSON attempt against the Bot API — `fetchOnce` in telegram.ts.
    /// `Err` means a transport-level failure (no usable response), which
    /// is the only case `call` retries.
    async fn fetch_once<T: DeserializeOwned>(
        &self,
        method: &str,
        payload: &serde_json::Value,
    ) -> std::result::Result<(u16, ApiEnvelope<T>), String> {
        let res = self
            .client
            .post(self.api_url(method))
            .json(payload)
            .send()
            .await
            .map_err(|e| self.redact(e.to_string()))?;
        let status = res.status().as_u16();
        let body: ApiEnvelope<T> = res.json().await.map_err(|e| self.redact(e.to_string()))?;
        Ok((status, body))
    }

    /// Low-level Bot API call — `tgCall` in telegram.ts.
    ///
    /// Never panics on API errors: transport failures retry once after
    /// 700ms; any received response is returned immediately (never
    /// replayed), with `Err(description || "HTTP <status>")` on `ok:false`.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        payload: &serde_json::Value,
    ) -> TgResult<T> {
        for attempt in 0..2 {
            match self.fetch_once(method, payload).await {
                Err(err) => {
                    if attempt == 0 {
                        tokio::time::sleep(Duration::from_millis(700)).await;
                        continue;
                    }
                    return Err(err);
                }
                Ok((status, body)) => {
                    return match (body.ok, body.result) {
                        (true, Some(result)) => Ok(result),
                        _ => Err(body.description.unwrap_or_else(|| format!("HTTP {status}"))),
                    };
                }
            }
        }
        Err("unreachable".to_string())
    }

    /// One multipart attempt (file upload) — `uploadOnce` in telegram.ts.
    async fn upload_once<T: DeserializeOwned>(
        &self,
        method: &str,
        form: Form,
    ) -> std::result::Result<(u16, ApiEnvelope<T>), String> {
        let res = self
            .client
            .post(self.api_url(method))
            .timeout(UPLOAD_TIMEOUT)
            .multipart(form)
            .send()
            .await
            .map_err(|e| self.redact(e.to_string()))?;
        let status = res.status().as_u16();
        let body: ApiEnvelope<T> = res.json().await.map_err(|e| self.redact(e.to_string()))?;
        Ok((status, body))
    }

    /// Multipart Bot API call — `tgUpload` in telegram.ts. Same retry
    /// semantics as `call`: transport failures retry once (1s), a
    /// received response is never replayed.
    ///
    /// `reqwest::multipart::Form` is consumed per request and is not
    /// `Clone`, so callers pass a factory — each attempt rebuilds the
    /// form, exactly like TS rebuilding `FormData` per `tgUpload` call.
    pub async fn upload<T, F>(&self, method: &str, make_form: F) -> TgResult<T>
    where
        T: DeserializeOwned,
        F: Fn() -> Form,
    {
        for attempt in 0..2 {
            match self.upload_once(method, make_form()).await {
                Err(err) => {
                    if attempt == 0 {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                    return Err(err);
                }
                Ok((status, body)) => {
                    return match (body.ok, body.result) {
                        (true, Some(result)) => Ok(result),
                        _ => Err(body.description.unwrap_or_else(|| format!("HTTP {status}"))),
                    };
                }
            }
        }
        Err("unreachable".to_string())
    }

    // ── send* helpers ────────────────────────────────────────────────

    /// `sendMessage` — chat_id accepts i64 or "@channel" strings like TS.
    pub async fn send_message(
        &self,
        chat_id: impl Serialize,
        text: &str,
        extra: Option<&serde_json::Value>,
    ) -> TgResult<TgMessageOut> {
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "HTML",
        });
        merge_extra(&mut payload, extra);
        self.call("sendMessage", &payload).await
    }

    /// `editMessageText`.
    pub async fn edit_message_text(
        &self,
        chat_id: impl Serialize,
        message_id: i64,
        text: &str,
        extra: Option<&serde_json::Value>,
    ) -> TgResult<TgMessageOut> {
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });
        merge_extra(&mut payload, extra);
        self.call("editMessageText", &payload).await
    }

    /// `editMessageReplyMarkup` — result shape varies (Message|true),
    /// so it stays a raw JSON value like the TS `tgCall<unknown>`.
    pub async fn edit_message_reply_markup(
        &self,
        chat_id: impl Serialize,
        message_id: i64,
        extra: Option<&serde_json::Value>,
    ) -> TgResult<serde_json::Value> {
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
        });
        merge_extra(&mut payload, extra);
        self.call("editMessageReplyMarkup", &payload).await
    }

    /// `answerCallbackQuery` — dismiss the spinner; optional toast text.
    pub async fn answer_callback_query(&self, id: &str, text: Option<&str>) -> TgResult<bool> {
        let mut payload = serde_json::json!({
            "callback_query_id": id,
        });
        if let Some(text) = text {
            payload["text"] = serde_json::json!(text);
            payload["show_alert"] = serde_json::json!(false);
        }
        self.call("answerCallbackQuery", &payload).await
    }

    /// `sendPhoto` by file_id.
    pub async fn send_photo(
        &self,
        chat_id: impl Serialize,
        file_id: &str,
        extra: Option<&serde_json::Value>,
    ) -> TgResult<TgMessageOut> {
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "photo": file_id,
        });
        merge_extra(&mut payload, extra);
        self.call("sendPhoto", &payload).await
    }

    /// `sendVideo` by file_id.
    pub async fn send_video(
        &self,
        chat_id: impl Serialize,
        file_id: &str,
        extra: Option<&serde_json::Value>,
    ) -> TgResult<TgMessageOut> {
        let mut payload = serde_json::json!({
            "chat_id": chat_id,
            "video": file_id,
        });
        merge_extra(&mut payload, extra);
        self.call("sendVideo", &payload).await
    }

    /// `sendMediaGroup` — deliver 2-10 items as one album.
    pub async fn send_media_group(
        &self,
        chat_id: impl Serialize,
        media: &[MediaGroupItem],
    ) -> TgResult<Vec<TgMessageOut>> {
        let payload = serde_json::json!({
            "chat_id": chat_id,
            "media": media,
        });
        self.call("sendMediaGroup", &payload).await
    }

    /// `getWebhookInfo`.
    pub async fn get_webhook_info(&self) -> TgResult<WebhookInfo> {
        self.call("getWebhookInfo", &serde_json::json!({})).await
    }

    /// `getMe` — smoke test / bot identity.
    pub async fn get_me(&self) -> TgResult<TgUser> {
        self.call("getMe", &serde_json::json!({})).await
    }

    // ── file resolution + streaming ──────────────────────────────────

    /// `locateTelegramFile` — resolve a file_id to its Bot API download
    /// path plus the derivable cache key and MIME guess.
    async fn locate_telegram_file(&self, file_id: &str) -> Result<LocatedFile, String> {
        let meta: FileResult = self
            .call("getFile", &serde_json::json!({ "file_id": file_id }))
            .await?;
        let Some(file_path) = meta.file_path else {
            return Err("no file_path".to_string());
        };
        let ext = ext_of(&file_path);
        let key = cache_key(file_id, &ext);
        let mime = mime_for_ext(&ext.to_lowercase())
            .map(str::to_string)
            .or_else(|| {
                file_path
                    .starts_with("photos/")
                    .then(|| "image/jpeg".to_string())
            });
        Ok(LocatedFile {
            file_path,
            file_size: meta.file_size,
            key,
            mime,
        })
    }

    /// `openTelegramFile` — open a live stream to a file_id's Telegram
    /// CDN copy. Pass `range` to forward a Range header upstream
    /// (Telegram's CDN honors ranges). The returned `reqwest::Response`
    /// IS the stream — feed `bytes_stream()` into `Body::from_stream`.
    pub async fn open_telegram_file(
        &self,
        file_id: &str,
        range: Option<&str>,
    ) -> Result<TelegramFile, String> {
        let loc = self.locate_telegram_file(file_id).await?;
        let mut req = self
            .client
            .get(self.file_url(&loc.file_path))
            .timeout(FILE_TIMEOUT);
        if let Some(range) = range {
            req = req.header("range", range);
        }
        let res = req.send().await.map_err(|e| self.redact(e.to_string()))?;
        if !res.status().is_success() {
            return Err(format!("HTTP {}", res.status()));
        }

        let status = res.status().as_u16();
        let header_str = |name: &str| {
            res.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        // content-length || file_size, treating 0/unparseable as absent —
        // mirrors `(Number(...) || fileSize) ?? null` in telegram.ts.
        let size = header_str("content-length")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .or(loc.file_size);

        Ok(TelegramFile {
            key: loc.key,
            mime: loc.mime,
            size,
            status,
            content_range: header_str("content-range"),
            response: res,
        })
    }

    /// `ensureCached` — resolve a file_id into the media cache,
    /// downloading from Telegram once. Bot API caps downloads at ~20 MB
    /// so buffering the body is safe (same reasoning as telegram.ts).
    /// Returns `None` on any failure — callers treat it as a soft miss.
    pub async fn ensure_cached(&self, file_id: &str, cache: &CacheBackend) -> Option<FileMeta> {
        let loc = match self.locate_telegram_file(file_id).await {
            Ok(loc) => loc,
            Err(err) => {
                tracing::warn!(file_id, error = %err, "ensure_cached: locate failed");
                return None;
            }
        };

        match cache.head(&loc.key).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                let res = self
                    .client
                    .get(self.file_url(&loc.file_path))
                    .timeout(FILE_TIMEOUT)
                    .send()
                    .await;
                let Ok(res) = res else {
                    tracing::warn!(file_id, "ensure_cached: download failed");
                    return None;
                };
                if !res.status().is_success() {
                    tracing::warn!(file_id, status = %res.status(), "ensure_cached: HTTP error");
                    return None;
                }
                let Ok(bytes) = res.bytes().await else {
                    return None;
                };
                if cache
                    .put(&loc.key, &bytes, loc.mime.as_deref())
                    .await
                    .is_err()
                {
                    return None;
                }
            }
            Err(err) => {
                tracing::warn!(file_id, error = %err, "ensure_cached: cache head failed");
                return None;
            }
        }

        Some(FileMeta {
            mime: loc.mime,
            key: loc.key,
        })
    }
}

// ── Supporting types ─────────────────────────────────────────────────

/// Result of `locateTelegramFile`.
pub struct LocatedFile {
    pub file_path: String,
    pub file_size: Option<u64>,
    pub key: String,
    pub mime: Option<String>,
}

/// `TelegramFile` — a live stream to a file_id's Telegram CDN copy.
pub struct TelegramFile {
    pub key: String,
    pub mime: Option<String>,
    /// Bytes this response will deliver (upstream Content-Length or file_size).
    pub size: Option<u64>,
    /// Upstream status — 206 when a Range request was honored, else 200.
    pub status: u16,
    pub content_range: Option<String>,
    /// The response body stream.
    pub response: reqwest::Response,
}

/// `FileMeta` — cache reference returned by `ensureCached`.
pub struct FileMeta {
    pub mime: Option<String>,
    pub key: String,
}

// ── Free helpers (ported 1:1 from telegram.ts) ───────────────────────

/// `file_id`/send errors meaning the underlying Telegram file is
/// unrecoverable — rows hitting these get flagged `media.missing`.
pub fn is_dead_file_error(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("file_id") || e.contains("file_reference") || e.contains("file is temporarily")
}

/// Photos arrive in several sizes; big document uploads can exceed the
/// Bot API download range (~20 MB).
pub fn is_streamable_size(bytes: Option<u64>) -> bool {
    bytes.is_none_or(|b| b > 0 && b <= MAX_STREAMABLE_BYTES)
}

/// `displayName` — first+last, else @username, else `user<id>`, else "friend".
pub fn display_name(user: Option<&TgUser>) -> String {
    let Some(u) = user else {
        return "friend".to_string();
    };
    let name = [u.first_name.as_deref(), u.last_name.as_deref()]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !name.is_empty() {
        return name;
    }
    if let Some(username) = &u.username {
        return format!("@{username}");
    }
    format!("user{}", u.id)
}

/// `escapeHtml` — escapes exactly `&`, `<`, `>` like the TS version
/// (Telegram reply text is HTML — `<code>`/`<b>` only allowed).
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// An inline-keyboard button — `{text, data}` callback or `{text, url}`.
pub struct InlineButton {
    pub text: String,
    pub data: Option<String>,
    pub url: Option<String>,
}

impl InlineButton {
    pub fn callback(text: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            data: Some(data.into()),
            url: None,
        }
    }

    pub fn link(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            data: None,
            url: Some(url.into()),
        }
    }
}

/// `inlineKb` — build a `reply_markup` payload value for inline keyboards.
/// URL buttons win over callback data, matching the TS ternary.
pub fn inline_kb(rows: &[Vec<InlineButton>]) -> serde_json::Value {
    let buttons: Vec<Vec<serde_json::Value>> = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|b| {
                    let mut btn = serde_json::json!({ "text": b.text });
                    if let Some(url) = &b.url {
                        btn["url"] = serde_json::json!(url);
                    } else if let Some(data) = &b.data {
                        btn["callback_data"] = serde_json::json!(data);
                    }
                    btn
                })
                .collect()
        })
        .collect();
    serde_json::json!({ "reply_markup": { "inline_keyboard": buttons } })
}

/// `fileForm` — build a multipart form for send* methods carrying a raw
/// file as one field. `extra` values are stringified like TS `String(v)`:
/// strings verbatim, other JSON scalars via `to_string`, objects as
/// compact JSON (slightly better than JS's `"[object Object]"`).
pub fn file_form(
    chat_id: impl Serialize,
    field: impl Into<String>,
    bytes: &[u8],
    mime: &str,
    filename: &str,
    extra: Option<&serde_json::Value>,
) -> Form {
    // JS String(chatId): numbers -> "123", strings -> verbatim.
    let chat_id_str = match serde_json::to_value(&chat_id) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    };

    // Invalid MIME strings degrade to an untyped part rather than panic.
    let part = match Part::bytes(bytes.to_vec())
        .file_name(filename.to_string())
        .mime_str(mime)
    {
        Ok(p) => p,
        Err(_) => Part::bytes(bytes.to_vec()).file_name(filename.to_string()),
    };

    let mut form = Form::new()
        .text("chat_id", chat_id_str)
        .part(field.into(), part);

    if let Some(serde_json::Value::Object(map)) = extra {
        for (k, v) in map {
            let value = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            form = form.text(k.clone(), value);
        }
    }
    form
}

/// Spread an `extra` object into a JSON payload object — the TS `...extra`
/// equivalent. Non-object extras are ignored.
fn merge_extra(payload: &mut serde_json::Value, extra: Option<&serde_json::Value>) {
    if let (Some(obj), Some(serde_json::Value::Object(map))) = (payload.as_object_mut(), extra) {
        for (k, v) in map {
            obj.insert(k.clone(), v.clone());
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_matches_ts_layout() {
        // sha1("abc") is a well-known vector.
        assert_eq!(
            cache_key("abc", ".jpg"),
            "a9993e364706816aba3e25717850c26c9cd0d89d.jpg"
        );
        assert_eq!(
            cache_key("abc", ""),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[test]
    fn error_strings_never_carry_the_token() {
        // reqwest errors embed the request URL — and every Bot API URL
        // contains the token. redact() must strip it from anything that
        // could reach a log line or an API error response.
        let tg = TgClient::new("12345:super-secret-token");
        for raw in [
            format!(
                "error sending request for url ({})",
                tg.api_url("sendPhoto")
            ),
            format!(
                "error decoding response body: timeout for url ({})",
                tg.api_url("getFile")
            ),
            format!(
                "HTTP status client error (400) for url ({})",
                tg.file_url("photos/file_1.jpg")
            ),
        ] {
            let out = tg.redact(raw);
            assert!(!out.contains("12345:super-secret-token"), "leaked: {out}");
            assert!(out.contains("[redacted]"), "no marker: {out}");
        }
        // untouched when the token isn't present
        assert_eq!(tg.redact("plain error".to_string()), "plain error");
    }

    #[test]
    fn dead_file_error_detection() {
        assert!(is_dead_file_error("Bad Request: file_id is too old"));
        assert!(is_dead_file_error("file is temporarily unavailable"));
        assert!(is_dead_file_error("wrong FILE_REFERENCE provided"));
        assert!(!is_dead_file_error("chat not found"));
        assert!(!is_dead_file_error(""));
    }

    #[test]
    fn streamable_size_boundaries() {
        assert!(is_streamable_size(None));
        assert!(!is_streamable_size(Some(0)));
        assert!(is_streamable_size(Some(1)));
        assert!(is_streamable_size(Some(MAX_STREAMABLE_BYTES)));
        assert!(!is_streamable_size(Some(MAX_STREAMABLE_BYTES + 1)));
    }

    #[test]
    fn display_name_fallbacks() {
        let user =
            |first: Option<&str>, last: Option<&str>, username: Option<&str>, id: i64| TgUser {
                id,
                is_bot: None,
                username: username.map(str::to_string),
                first_name: first.map(str::to_string),
                last_name: last.map(str::to_string),
            };
        assert_eq!(display_name(None), "friend");
        assert_eq!(
            display_name(Some(&user(Some("Ada"), Some("Lovelace"), Some("ada"), 7))),
            "Ada Lovelace"
        );
        assert_eq!(
            display_name(Some(&user(None, None, Some("ada"), 7))),
            "@ada"
        );
        assert_eq!(display_name(Some(&user(None, None, None, 42))), "user42");
    }

    #[test]
    fn escape_html_escapes_amp_first() {
        assert_eq!(escape_html("a<b>&c"), "a&lt;b&gt;&amp;c");
        assert_eq!(escape_html("<b>ok</b>"), "&lt;b&gt;ok&lt;/b&gt;");
        assert_eq!(escape_html("plain"), "plain");
    }

    #[test]
    fn inline_kb_shape() {
        let kb = inline_kb(&[vec![
            InlineButton::callback("View", "view:1"),
            InlineButton::link("Open", "https://example.com"),
        ]]);
        assert_eq!(
            kb,
            serde_json::json!({
                "reply_markup": {
                    "inline_keyboard": [[
                        {"text": "View", "callback_data": "view:1"},
                        {"text": "Open", "url": "https://example.com"}
                    ]]
                }
            })
        );
    }

    #[test]
    fn ext_of_handles_dirs_and_case() {
        assert_eq!(ext_of("photos/file_1.JPG"), ".JPG");
        assert_eq!(ext_of("documents/report.final.pdf"), ".pdf");
        assert_eq!(ext_of("photos/noext"), "");
        assert_eq!(ext_of("dir.name/file"), "");
    }

    #[test]
    fn mime_lookup() {
        assert_eq!(mime_for_ext(".jpg"), Some("image/jpeg"));
        assert_eq!(mime_for_ext(".mp4"), Some("video/mp4"));
        assert_eq!(mime_for_ext(".pdf"), None);
    }
}

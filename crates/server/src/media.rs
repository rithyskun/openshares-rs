//! Media streaming — the `server` half of `src/lib/storage.ts` +
//! `src/app/api/media/[id]/route.ts` (the GET handler, L86-190).
//!
//! `serve_media` resolves a `file_id` to bytes: a cache hit streams from
//! `FsCache` with HTTP Range support (`206`), a miss opens the Telegram
//! CDN stream via `TgClient::open_telegram_file` and tees it through
//! `CacheWriter` so the object lands in cache for next time.
//! Responses MUST stream (`Body::from_stream`) — never buffer whole files.
//!
//! Route-side concerns stay with the caller (TS route.ts L63-99): slug
//! lookup, token auth, `?thumb=` file_id selection, and the
//! `Content-Disposition: attachment` header for `?download=1` — the
//! signature takes no download flag, so the route layers that header
//! onto the returned `Response`.

use std::io::SeekFrom;

use axum::body::Body;
use axum::http::StatusCode;
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::response::Response;
use openshares_core::error::AppError;
use openshares_core::media::{CacheObject, CacheWriter, FsCache, cache_key};
use openshares_core::telegram::{TelegramFile, TgClient, is_dead_file_error};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, DuplexStream};
use tokio_util::io::ReaderStream;

use crate::error::{ApiError, Result};
use crate::state::AppState;

/// `baseType` last-resort in route.ts — TS falls back to a kind-derived
/// mime (`image/jpeg`/`video/mp4`); this layer has no `kind`, so the
/// generic binary type is the honest default.
const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

/// Upstream pump chunk size AND the client-facing pipe capacity.
/// Small enough for first-byte latency, large enough to stay syscall-light.
const PUMP_BUF: usize = 64 * 1024;

/// Serve a media object by Telegram `file_id`.
///
/// - `range_header`: raw `Range` header value (`bytes=a-b`), forwarded
///   upstream on cache misses since Telegram's CDN honors ranges.
/// - `mime_hint`: DB-stored mime used when the cache sidecar has none.
///
/// Statuses: `200` full body, `206` partial, `416` unsatisfiable range,
/// `404` dead file_id (`is_dead_file_error`), `502` upstream failure,
/// `503` when `BOT_TOKEN` unset.
pub async fn serve_media(
    state: &AppState,
    file_id: &str,
    mime_hint: Option<&str>,
    range_header: Option<&str>,
) -> Result<Response> {
    // `range` in TS is a string — the empty string is falsy and skips the
    // whole Range path (no upstream forward, no local parsing).
    let range_header = range_header.filter(|r| !r.is_empty());
    let mime_hint = mime_hint.filter(|m| !m.is_empty());

    // route.ts L91-93: the cache key is derivable — try the guessed key
    // first (`cacheKey(fileId, extFromMime(media.mime))`). `mime_hint`
    // plays the `media.mime` role; an unmapped mime yields no guess,
    // exactly like TS `knownExt ? cacheKey(...) : null`.
    if let Some(ext) = mime_hint.and_then(ext_from_mime) {
        let key = cache_key(file_id, ext);
        match state.cache.open(&key).await {
            Ok(Some(obj)) => return serve_cached(obj, mime_hint, range_header).await,
            Ok(None) => {}
            // storage.ts swallows fs errors in cacheHead/cacheGet — a
            // broken entry degrades to a cold read, never a 500.
            Err(err) => {
                tracing::warn!(key = %key, error = %err, "media: cache open failed — cold read");
            }
        }
    }

    serve_cold(state, file_id, mime_hint, range_header).await
}

/// The `!head` branch of the TS GET handler (route.ts L102-147): open the
/// Telegram CDN stream, then either pass a `206` through or tee a `200`
/// into the cache.
async fn serve_cold(
    state: &AppState,
    file_id: &str,
    mime_hint: Option<&str>,
    range_header: Option<&str>,
) -> Result<Response> {
    // Only the cold path needs the bot token — a cache hit must serve
    // even with BOT_TOKEN unset (TS only calls botToken() inside
    // openTelegramFile/ensureCached).
    let tg = state.require_tg().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Telegram storage is not configured.",
        )
    })?;

    // openTelegramFile — Err carries the Bot API description or
    // "HTTP <status>" (upstream non-success already folded in).
    let opened = tg
        .open_telegram_file(file_id, range_header)
        .await
        .map_err(|err| map_upstream_error(file_id, &err))?;

    serve_telegram(tg, &state.cache, file_id, opened, mime_hint, range_header)
}

/// route.ts L108-117 — dead file_ids get a not-found (the TS route marks
/// `media.missing` and returns 410; the DB flag is the caller's concern,
/// so we surface the message as 404). Everything else is a 502
/// "Telegram storage unavailable.".
fn map_upstream_error(file_id: &str, err: &str) -> ApiError {
    if is_dead_file_error(err) {
        ApiError::not_found(
            "The original Telegram file is no longer reachable (bot changed or message deleted).",
        )
    } else {
        tracing::warn!(file_id, error = %err, "media: open_telegram_file failed");
        ApiError::new(StatusCode::BAD_GATEWAY, "Telegram storage unavailable.")
    }
}

/// Dispatch on the upstream response — the two cold paths in route.ts.
fn serve_telegram(
    tg: &TgClient,
    cache: &FsCache,
    file_id: &str,
    opened: TelegramFile,
    mime_hint: Option<&str>,
    range_header: Option<&str>,
) -> Result<Response> {
    // route.ts L119: contentType = media.mime ?? tg.mime ?? baseType.
    let content_type = mime_hint
        .or(opened.mime.as_deref())
        .unwrap_or(DEFAULT_CONTENT_TYPE)
        .to_string();

    // TS `tg.contentRange` is truthiness-checked — an empty header value
    // takes the tee path, not passthrough.
    if range_header.is_some()
        && opened.status == 206
        && opened
            .content_range
            .as_deref()
            .is_some_and(|cr| !cr.is_empty())
    {
        // route.ts L120-134 — upstream honored the client's Range:
        // passthrough with the verbatim Content-Range, and warm the whole
        // object behind the response (`background(ensureCached(fileId))`).
        let (tg, cache, file_id) = (tg.clone(), cache.clone(), file_id.to_string());
        tokio::spawn(async move {
            if tg.ensure_cached(&file_id, &cache).await.is_none() {
                tracing::debug!(file_id = %file_id, "media: background ensure_cached failed");
            }
        });

        let mut res = Response::new(Body::from_stream(opened.response.bytes_stream()));
        *res.status_mut() = StatusCode::PARTIAL_CONTENT;
        put_headers(res.headers_mut(), &content_type, opened.size);
        if let Some(content_range) = &opened.content_range {
            put_header(res.headers_mut(), header::CONTENT_RANGE, content_range);
        }
        return Ok(res);
    }

    // route.ts L135-146 — `tg.stream.tee()`: one branch to the client,
    // one into `cachePut(tg.key, _, tg.mime ?? contentType, tg.size)`.
    // The pump below is the single-pass equivalent: each upstream chunk
    // goes to the client pipe and the `.part` writer together.
    let cache_mime = opened.mime.clone().unwrap_or_else(|| content_type.clone());
    let size = opened.size;
    let body = tee_body(opened, cache.clone(), cache_mime);
    let mut res = Response::new(body);
    *res.status_mut() = StatusCode::OK;
    put_headers(res.headers_mut(), &content_type, size);
    Ok(res)
}

/// Cache-hit branch — route.ts L149-189. `obj` is an open handle on the
/// object plus its size/sidecar mime (`cacheGet` equivalent).
async fn serve_cached(
    mut obj: CacheObject,
    mime_hint: Option<&str>,
    range_header: Option<&str>,
) -> Result<Response> {
    let size = obj.size;
    // sidecar mime → mime_hint → octet-stream. (TS orders `media.mime`
    // first — the sidecar was itself written from that mime, so the two
    // orders only diverge if the DB mime changed after caching.)
    let content_type = obj
        .mime
        .clone()
        .or_else(|| mime_hint.map(str::to_string))
        .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());

    if let Some(range) = range_header {
        return match resolve_range(range, size) {
            ResolvedRange::Satisfiable { start, end } => {
                obj.file
                    .seek(SeekFrom::Start(start))
                    .await
                    .map_err(AppError::Io)
                    .map_err(ApiError::from)?;
                let len = end - start + 1;
                let body = Body::from_stream(ReaderStream::new(obj.file.take(len)));

                let mut res = Response::new(body);
                *res.status_mut() = StatusCode::PARTIAL_CONTENT;
                put_headers(res.headers_mut(), &content_type, Some(len));
                put_header(
                    res.headers_mut(),
                    header::CONTENT_RANGE,
                    format_args!("bytes {start}-{end}/{size}"),
                );
                Ok(res)
            }
            ResolvedRange::Unsatisfiable => {
                // route.ts L161 — 416 with the `bytes */size` form.
                let mut res = Response::new(Body::empty());
                *res.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                put_header(
                    res.headers_mut(),
                    header::CONTENT_RANGE,
                    format_args!("bytes */{size}"),
                );
                Ok(res)
            }
        };
    }

    let body = Body::from_stream(ReaderStream::new(obj.file));
    let mut res = Response::new(body);
    *res.status_mut() = StatusCode::OK;
    put_headers(res.headers_mut(), &content_type, Some(size));
    Ok(res)
}

// ── Range parsing (route.ts L154-162) ────────────────────────────────

/// What the `Range` header resolves to for an object of `size` bytes.
enum ResolvedRange {
    /// Inclusive byte span — always satisfiable when produced.
    Satisfiable { start: u64, end: u64 },
    /// Out of bounds → `416` + `Content-Range: bytes */size`.
    Unsatisfiable,
}

/// One `bytes=\d*-\d*` match — `None` members mirror the TS regex's
/// empty (falsy) capture groups, which fall back to the open end.
struct RangeMatch {
    start: Option<u64>,
    end: Option<u64>,
}

/// The TS algorithm verbatim: `/bytes=(\d*)-(\d*)/` on the raw header
/// (unanchored — later `bytes=` occurrences can still match), missing
/// groups default to the open ends, out-of-bounds → 416. Deliberate
/// quirks preserved: a header with NO match serves the whole object as a
/// 206 (TS never "ignores" a Range), and `bytes=-b` is read as `0-b`,
/// not an HTTP suffix range.
fn resolve_range(range: &str, size: u64) -> ResolvedRange {
    // TS computes end = size-1 = -1 on an empty object → start > end →
    // 416. Rust u64 can't express that, so guard up front.
    if size == 0 {
        return ResolvedRange::Unsatisfiable;
    }
    let last = size - 1;
    let m = match_byte_range(range).unwrap_or(RangeMatch {
        start: None,
        end: None,
    });
    let start = m.start.unwrap_or(0);
    let end = m.end.map_or(last, |e| e.min(last));
    if start > end || start >= size {
        ResolvedRange::Unsatisfiable
    } else {
        ResolvedRange::Satisfiable { start, end }
    }
}

/// `/bytes=(\d*)-(\d*)/.exec(range)` — scans for the FIRST occurrence of
/// `bytes=` followed by `\d*-\d*`; a `bytes=` not followed by `-` is
/// skipped (regex backtracking tries the next position). Captures parse
/// like `parseInt`: pure digits, overflow saturates to `u64::MAX` (the
/// bounds check clamps/rejects it the same way TS's huge float does).
fn match_byte_range(range: &str) -> Option<RangeMatch> {
    let bytes = range.as_bytes();
    let digits = |from: usize, to: usize| -> Option<u64> {
        if from == to {
            return None;
        }
        let mut v = 0u64;
        for &b in &bytes[from..to] {
            v = v.saturating_mul(10).saturating_add(u64::from(b - b'0'));
        }
        Some(v)
    };

    let mut search_from = 0usize;
    while let Some(rel) = range.get(search_from..)?.find("bytes=") {
        let group = search_from + rel + "bytes=".len();
        let mut i = group;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if bytes.get(i) == Some(&b'-') {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            return Some(RangeMatch {
                start: digits(group, i),
                end: digits(i + 1, j),
            });
        }
        search_from = group; // regex resumes scanning right of this "bytes="
    }
    None
}

/// `extFromMime` in route.ts — mime → extension, for the derivable cache
/// key (and the caller's `?download=1` filename).
pub fn ext_from_mime(mime: &str) -> Option<&'static str> {
    match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" => Some(".jpg"),
        "image/png" => Some(".png"),
        "image/webp" => Some(".webp"),
        "image/gif" => Some(".gif"),
        "image/heic" => Some(".heic"),
        "video/mp4" => Some(".mp4"),
        "video/quicktime" => Some(".mov"),
        "video/webm" => Some(".webm"),
        "video/x-matroska" => Some(".mkv"),
        "video/x-msvideo" => Some(".avi"),
        _ => None,
    }
}

// ── The tee (route.ts L135-136 + `cachePut` in storage.ts) ────────────

/// Build the client body for a cold `200`: a spawned pump forwards every
/// upstream chunk to a duplex pipe (→ `ReaderStream` → hyper) and to a
/// `.part` cache write in one pass — first-byte latency, never buffered.
/// Takes the whole `TelegramFile` so this crate never has to name the
/// `reqwest` type (reqwest is a core-crate dep, not a server one).
///
/// NOTE: `duplex`, not `simplex` — in tokio 1.54 `simplex()` returns
/// `split()` halves whose Drop does NOT propagate reader-drop to the
/// writer, so a client disconnect would hang the pump. `DuplexStream`'s
/// Drop calls `close_read`/`close_write`: dropping the read end makes
/// the pending `write_all` fail with `BrokenPipe` — the disconnect
/// signal the abort logic depends on.
fn tee_body(file: TelegramFile, cache: FsCache, mime: String) -> Body {
    let (reader, writer) = tokio::io::duplex(PUMP_BUF);
    tokio::spawn(tee_pump_telegram(file, writer, cache, mime));
    Body::from_stream(ReaderStream::new(reader))
}

/// Production half of the tee — one pass over `TelegramFile::response`
/// via `chunk()` (the non-Stream reqwest reader; `bytes_stream()` can't
/// be adapted without a `futures` dep — `StreamReader` needs
/// `Into<io::Error>` which `reqwest::Error` doesn't implement). Every
/// chunk goes to the client pipe AND the `.part` cache writer (the
/// `stream.tee()` + `cachePut` pair in TS).
///
/// Cache failures never fail the client — `cachePut` is best-effort
/// background work in TS too (a mid-stream write error drops the put
/// and keeps serving). Client disconnect or upstream error aborts the
/// put and removes `.part` — the TS tee lets the cache branch finish
/// anyway, but a half-file in cache is worse than a re-fetch.
/// `PutGuard` covers the remaining hole: pump task cancellation.
async fn tee_pump_telegram(
    mut file: TelegramFile,
    mut client: DuplexStream,
    cache: FsCache,
    mime: String,
) {
    let key = file.key.clone();
    let mut writer = open_writer(&cache, &key).await;
    loop {
        match file.response.chunk().await {
            Ok(Some(bytes)) => {
                if !pump_chunk(bytes.as_ref(), &mut client, &mut writer, &key).await {
                    abort_put(&mut writer).await;
                    return;
                }
            }
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(key = %key, error = %err, "media tee: upstream read failed — aborting put");
                abort_put(&mut writer).await;
                return;
            }
        }
    }
    finish_pump(&mut writer, &mut client, Some(&mime)).await;
}

/// Open the `.part` writer — a cache-open failure only logs (the client
/// still gets every byte; `cachePut` is best-effort in TS too).
async fn open_writer(cache: &FsCache, key: &str) -> Option<PutGuard> {
    match cache.begin_put(key).await {
        Ok(w) => Some(PutGuard::new(w)),
        Err(err) => {
            tracing::warn!(key = %key, error = %err, "media tee: begin_put failed — streaming uncached");
            None
        }
    }
}

/// One upstream chunk through the tee: cache-write first (best-effort —
/// a failed put is aborted and the client stream continues, mirroring
/// the independent tee branches in TS), then the client pipe. Returns
/// `false` when the client is gone — the caller aborts the put.
async fn pump_chunk(
    chunk: &[u8],
    client: &mut DuplexStream,
    writer: &mut Option<PutGuard>,
    key: &str,
) -> bool {
    if let Some(guard) = writer.as_mut() {
        match guard.write_chunk(chunk).await {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!(key = %key, error = %err, "media tee: cache write failed — dropping put, client unaffected");
                abort_put(writer).await;
            }
        }
    }
    if let Err(err) = client.write_all(chunk).await {
        tracing::debug!(key = %key, error = %err, "media tee: client disconnected — aborting put");
        return false;
    }
    true
}

/// Abort the put (`take` disarms the guard so Drop doesn't double-abort).
async fn abort_put(writer: &mut Option<PutGuard>) {
    if let Some(g) = writer.take() {
        g.abort().await;
    }
}

/// Clean end of a pump: commit the cache write, then close the client
/// pipe so the body sees EOF (dropping `client` works too, but this is
/// explicit about the ordering).
async fn finish_pump(writer: &mut Option<PutGuard>, client: &mut DuplexStream, mime: Option<&str>) {
    if let Some(g) = writer.take() {
        g.finish(mime).await;
    }
    let _ = client.shutdown().await;
}

// ── Response headers (the constant set in route.ts) ──────────────────

/// The headers every streaming response carries: Content-Type,
/// Accept-Ranges, Cache-Control, and Content-Length when the byte count
/// is known (TS: `...(tg.size ? { "Content-Length": ... } : {})`).
fn put_headers(h: &mut HeaderMap, content_type: &str, size: Option<u64>) {
    put_header(h, header::CONTENT_TYPE, content_type);
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=86400"),
    );
    if let Some(size) = size.filter(|s| *s > 0) {
        put_header(h, header::CONTENT_LENGTH, size);
    }
}

/// Insert a header, logging + skipping values that can't encode (a bad
/// optional header must never sink a response TS would have sent).
fn put_header(h: &mut HeaderMap, name: HeaderName, value: impl std::fmt::Display) {
    match HeaderValue::from_str(&value.to_string()) {
        Ok(v) => {
            h.insert(name, v);
        }
        Err(_) => tracing::warn!(%name, "media: dropped unencodable header value"),
    }
}

// ── `.part` cleanup ─────────────────────────────────────────────────

/// A `CacheWriter` that aborts (removes `.part`) on drop unless disarmed
/// by `finish`/`abort` — so a cancelled pump task can never leave a
/// stale partial file behind.
struct PutGuard {
    writer: Option<CacheWriter>,
}

impl PutGuard {
    fn new(writer: CacheWriter) -> Self {
        Self {
            writer: Some(writer),
        }
    }

    async fn write_chunk(&mut self, chunk: &[u8]) -> openshares_core::error::Result<()> {
        match self.writer.as_mut() {
            Some(w) => w.write_chunk(chunk).await,
            None => Ok(()),
        }
    }

    /// Commit — rename `.part` → key + write the mime sidecar.
    /// Disarmed: the writer is taken, so Drop is a no-op.
    async fn finish(mut self, mime: Option<&str>) {
        if let Some(w) = self.writer.take() {
            match w.finish(mime).await {
                Ok(bytes) => tracing::debug!(bytes, "media tee: cache put committed"),
                Err(err) => tracing::warn!(error = %err, "media tee: cache finish failed"),
            }
        }
    }

    /// Abandon the put — removes `.part`. Awaited inline so the file is
    /// gone before callers assert state; the Drop fallback only fires on
    /// task cancellation, where awaiting isn't an option.
    async fn abort(mut self) {
        if let Some(w) = self.writer.take() {
            w.abort().await;
        }
    }
}

impl Drop for PutGuard {
    fn drop(&mut self) {
        if let Some(w) = self.writer.take() {
            // Can't await in Drop — detach the removal. `try_current`
            // fails only outside the runtime (process exit), where a
            // stale `.part` no longer matters.
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move { w.abort().await });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, ReadBuf};

    /// The pump loop over a generic `AsyncRead` — tests fake the upstream
    /// with in-memory sources; `tee_pump_telegram` can't be driven
    /// without a live CDN response. Shares every helper with production.
    async fn tee_pump<R>(
        mut upstream: R,
        mut client: DuplexStream,
        cache: FsCache,
        key: String,
        mime: String,
    ) where
        R: AsyncRead + Unpin,
    {
        let mut writer = open_writer(&cache, &key).await;
        let mut buf = vec![0u8; PUMP_BUF].into_boxed_slice();
        loop {
            match upstream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if !pump_chunk(&buf[..n], &mut client, &mut writer, &key).await {
                        abort_put(&mut writer).await;
                        return;
                    }
                }
                Err(err) => {
                    tracing::warn!(key = %key, error = %err, "media tee(test): upstream failed — aborting put");
                    abort_put(&mut writer).await;
                    return;
                }
            }
        }
        finish_pump(&mut writer, &mut client, Some(&mime)).await;
    }

    // ── resolve_range — the TS regex semantics, case by case ─────────

    fn range(h: &str, size: u64) -> Option<(u64, u64)> {
        match resolve_range(h, size) {
            ResolvedRange::Satisfiable { start, end } => Some((start, end)),
            ResolvedRange::Unsatisfiable => None,
        }
    }

    #[test]
    fn range_full() {
        assert_eq!(range("bytes=0-99", 200), Some((0, 99)));
        // end beyond size clamps to size-1
        assert_eq!(range("bytes=0-999", 200), Some((0, 199)));
        assert_eq!(range("bytes=199-199", 200), Some((199, 199)));
    }

    #[test]
    fn range_open_start_and_end() {
        assert_eq!(range("bytes=100-", 200), Some((100, 199)));
        // TS quirk: `bytes=-50` is 0-50, NOT the last 50 bytes
        assert_eq!(range("bytes=-50", 200), Some((0, 50)));
        // `bytes=` — both groups empty → whole object
        assert_eq!(range("bytes=-", 200), Some((0, 199)));
        assert_eq!(range("bytes=", 200), Some((0, 199)));
    }

    #[test]
    fn range_unsatisfiable() {
        assert_eq!(range("bytes=5-3", 200), None); // start > end
        assert_eq!(range("bytes=200-", 200), None); // start == size
        assert_eq!(range("bytes=250-", 200), None); // start > size
        assert_eq!(range("bytes=0-0", 0), None); // empty object
        assert_eq!(range("bytes=-", 0), None); // empty object
    }

    #[test]
    fn range_parse_oddities_match_ts_regex() {
        // No `bytes=\d*-\d*` match at all → whole object 206 (TS never
        // "ignores" a Range header it can't parse).
        assert_eq!(range("garbage", 200), Some((0, 199)));
        assert_eq!(range("items=0-10", 200), Some((0, 199)));
        assert_eq!(range("bytes", 200), Some((0, 199)));
        // Unanchored: matches inside other text, first match wins.
        assert_eq!(range("xbytes=2-4", 200), Some((2, 4)));
        assert_eq!(range("bytes=1-2,bytes=3-4", 200), Some((1, 2)));
        // A `bytes=` not followed by `-` is skipped, next one matches.
        assert_eq!(range("bytes=,bytes=3-7", 200), Some((3, 7)));
        assert_eq!(range("bytes=abc bytes=3-7", 200), Some((3, 7)));
        // Trailing junk after the second group is fine.
        assert_eq!(range("bytes=1-2-3", 200), Some((1, 2)));
        // Overflow saturates like a huge parseInt → rejected by bounds.
        assert_eq!(range("bytes=99999999999999999999999-", 200), None);
        assert_eq!(
            range("bytes=0-99999999999999999999999", 200),
            Some((0, 199))
        );
    }

    #[test]
    fn ext_mapping_matches_ts_table() {
        assert_eq!(ext_from_mime("image/jpeg"), Some(".jpg"));
        assert_eq!(ext_from_mime("VIDEO/MP4"), Some(".mp4")); // lowercase()d in TS
        assert_eq!(ext_from_mime("application/pdf"), None);
        assert_eq!(ext_from_mime("image/jpeg; charset=x"), None);
    }

    // ── the tee pump — real FsCache on a tempdir ──────────────────────

    /// Client half: drain the pipe reader like hyper would.
    async fn drain(mut rd: DuplexStream) -> Vec<u8> {
        let mut out = Vec::new();
        rd.read_to_end(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn tee_streams_to_client_and_commits_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path());
        let data: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();

        let (rd, wr) = tokio::io::duplex(1024); // small pipe → many pump passes
        let client = tokio::spawn(drain(rd));
        tee_pump(
            std::io::Cursor::new(data.clone()),
            wr,
            cache.clone(),
            "vid.mp4".to_string(),
            "video/mp4".to_string(),
        )
        .await;

        assert_eq!(client.await.unwrap(), data);
        // committed: object + mime sidecar, no .part
        assert_eq!(cache.read("vid.mp4").await.unwrap().unwrap(), data);
        assert_eq!(
            cache
                .head("vid.mp4")
                .await
                .unwrap()
                .unwrap()
                .mime
                .as_deref(),
            Some("video/mp4")
        );
        assert!(!dir.path().join("vid.mp4.part").exists());
    }

    #[tokio::test]
    async fn tee_aborts_part_file_on_client_disconnect() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path());
        let data = vec![7u8; 256 * 1024];

        let (mut rd, wr) = tokio::io::duplex(512);
        let cache2 = cache.clone();
        let pump = tokio::spawn(tee_pump(
            std::io::Cursor::new(data),
            wr,
            cache,
            "gone.bin".to_string(),
            "application/octet-stream".to_string(),
        ));

        // Read a little then hang up — like a closed browser tab.
        let mut small = [0u8; 64];
        rd.read_exact(&mut small).await.unwrap();
        drop(rd);
        pump.await.unwrap();

        // .part must not linger; nothing committed.
        assert!(cache2.head("gone.bin").await.unwrap().is_none());
        assert!(!dir.path().join("gone.bin.part").exists());
    }

    /// An upstream that yields chunks then fails — `Err` mid-read must
    /// abort the `.part` file (reqwest transport error equivalent).
    struct FlakyRead {
        chunks: VecDeque<std::io::Result<Vec<u8>>>,
    }

    impl AsyncRead for FlakyRead {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            match self.chunks.pop_front() {
                Some(Ok(bytes)) => {
                    buf.put_slice(&bytes);
                    Poll::Ready(Ok(()))
                }
                Some(Err(e)) => Poll::Ready(Err(e)),
                None => Poll::Ready(Ok(())),
            }
        }
    }

    #[tokio::test]
    async fn tee_aborts_part_file_on_upstream_error() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path());
        let upstream = FlakyRead {
            chunks: VecDeque::from([
                Ok(vec![1u8; 100]),
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "net died",
                )),
            ]),
        };

        let (rd, wr) = tokio::io::duplex(4096);
        let client = tokio::spawn(drain(rd));
        tee_pump(
            upstream,
            wr,
            cache.clone(),
            "flaky.bin".to_string(),
            "application/octet-stream".to_string(),
        )
        .await;
        // client sees the truncated prefix then EOF — never an error frame
        assert_eq!(client.await.unwrap(), vec![1u8; 100]);

        assert!(cache.head("flaky.bin").await.unwrap().is_none());
        assert!(!dir.path().join("flaky.bin.part").exists());
    }

    // ── serve_cached end-to-end on a real FsCache ─────────────────────

    async fn body_bytes(res: Response) -> Vec<u8> {
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    #[tokio::test]
    async fn cached_range_request_is_206_with_content_range() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path());
        let data: Vec<u8> = (0..100u8).collect();
        cache.put("k.mp4", &data, Some("video/mp4")).await.unwrap();
        let obj = cache.open("k.mp4").await.unwrap().unwrap();

        let res = serve_cached(obj, None, Some("bytes=10-19")).await.unwrap();
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        let h = res.headers();
        assert_eq!(h[header::CONTENT_RANGE], "bytes 10-19/100");
        assert_eq!(h[header::CONTENT_LENGTH], "10");
        assert_eq!(h[header::ACCEPT_RANGES], "bytes");
        assert_eq!(h[header::CACHE_CONTROL], "max-age=86400");
        assert_eq!(h[header::CONTENT_TYPE], "video/mp4");
        assert_eq!(body_bytes(res).await, data[10..20].to_vec());
    }

    #[tokio::test]
    async fn cached_full_request_is_200() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path());
        cache.put("k.jpg", b"pixels", None).await.unwrap();
        let obj = cache.open("k.jpg").await.unwrap().unwrap();

        let res = serve_cached(obj, Some("image/jpeg"), None).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let h = res.headers();
        assert_eq!(h[header::CONTENT_LENGTH], "6");
        assert_eq!(h[header::CONTENT_TYPE], "image/jpeg"); // mime_hint fills empty sidecar
        assert_eq!(body_bytes(res).await, b"pixels".to_vec());
    }

    #[tokio::test]
    async fn cached_bad_range_is_416() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path());
        cache.put("k.bin", b"0123456789", None).await.unwrap();
        let obj = cache.open("k.bin").await.unwrap().unwrap();

        let res = serve_cached(obj, None, Some("bytes=50-60")).await.unwrap();
        assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(res.headers()[header::CONTENT_RANGE], "bytes */10");
        assert!(body_bytes(res).await.is_empty());
    }
}

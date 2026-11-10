//! Blob cache + media streaming — port of `src/lib/storage.ts` and the
//! streaming logic behind `src/app/api/media/**`.
//!
//! `CacheBackend` selects the storage half of `storage.ts`:
//! `FsCache` is the `data/cache/` filesystem fallback (local dev, bot
//! scripts); `S3Cache` is the `MEDIA_CACHE` R2 bucket in-app. Both expose
//! the same `head`/`put`/`read`/`open`/`begin_put`/`sweep` surface so
//! call sites never branch on backend.

mod s3;

use std::path::PathBuf;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use sha1::{Digest, Sha1};
use std::fmt::Write as _;
use tokio::io::{AsyncRead, AsyncSeek, AsyncWriteExt};

use crate::config::Config;
use crate::error::{AppError, Result};

pub use s3::S3Cache;

/// Cache object key — mirrors `cacheKey(fileId, ext)` in storage.ts:
/// `sha1(file_id)` hex + extension, so a cached copy is found by
/// derivation alone.
pub fn cache_key(file_id: &str, ext: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(file_id.as_bytes());
    let digest = hasher.finalize();

    let mut key = String::with_capacity(40 + ext.len());
    for byte in digest {
        let _ = write!(key, "{byte:02x}");
    }
    key.push_str(ext);
    key
}

/// `CacheHead` — size + optional mime from the `.meta.json` sidecar.
#[derive(Debug, Clone)]
pub struct CacheHead {
    pub size: u64,
    pub mime: Option<String>,
}

/// Filesystem media cache — the `data/cache/` half of `storage.ts`.
/// Keys are `sha1(file_id)+ext` (safe charset); path separators and `..`
/// are rejected defensively before touching the filesystem.
#[derive(Clone, Debug)]
pub struct FsCache {
    root: PathBuf,
}

impl FsCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Object path for a key. R2-style keys may contain `/` (e.g. the
    /// `audit/` prefix); `..`, `\` and absolute paths are rejected so a
    /// key can never escape the cache root.
    fn path(&self, key: &str) -> Result<PathBuf> {
        validate_key(key)?;
        Ok(self.root.join(key))
    }

    fn sidecar(&self, key: &str) -> Result<PathBuf> {
        Ok(self.root.join(format!("{key}.meta.json")))
    }

    /// `cacheHead` — object size + mime sidecar, or `None` when absent.
    pub async fn head(&self, key: &str) -> Result<Option<CacheHead>> {
        let path = self.path(key)?;
        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        let mime = match tokio::fs::read_to_string(self.sidecar(key)?).await {
            Ok(raw) => serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .and_then(|v| v.get("mime")?.as_str().map(str::to_string)),
            Err(_) => None,
        };

        Ok(Some(CacheHead {
            size: meta.len(),
            mime,
        }))
    }

    /// `cachePut` — write bytes plus the `.meta.json` mime sidecar.
    /// (Streaming puts land with the media routes in phase 4/5.)
    pub async fn put(&self, key: &str, data: &[u8], mime: Option<&str>) -> Result<()> {
        let path = self.path(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, data).await?;
        if let Some(mime) = mime {
            let sidecar = serde_json::json!({ "mime": mime }).to_string();
            tokio::fs::write(self.sidecar(key)?, sidecar).await?;
        }
        Ok(())
    }

    /// Read a whole object — `None` when absent. Used by the audit
    /// append path.
    pub async fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path(key)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// `cacheGet` streaming form — open the object for ranged reads.
    /// The caller seeks (`AsyncSeekExt`) and limits (`AsyncReadExt::take`)
    /// to implement HTTP Range; `size`/`mime` feed the response headers.
    pub async fn open(&self, key: &str) -> Result<Option<CacheObject>> {
        let head = match self.head(key).await? {
            Some(h) => h,
            None => return Ok(None),
        };
        let file = tokio::fs::File::open(self.path(key)?).await?;
        Ok(Some(CacheObject {
            stream: Box::pin(file),
            size: head.size,
            mime: head.mime,
        }))
    }

    /// `cachePut` streaming form — write to `<key>.part`, then `finish`
    /// renames it into place and writes the mime sidecar. Used by the
    /// Telegram-CDN tee-to-cache path so a dropped stream never leaves a
    /// half-written object visible.
    pub async fn begin_put(&self, key: &str) -> Result<CacheWriter> {
        let part_path = self.path(&format!("{key}.part"))?;
        if let Some(parent) = part_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = tokio::fs::File::create(&part_path).await?;
        Ok(CacheWriter {
            inner: CacheWriterInner::Fs {
                file,
                part_path,
                final_path: self.path(key)?,
                sidecar_path: self.sidecar(key)?,
            },
            bytes: 0,
        })
    }

    /// TTL sweep — port of `runCacheCleanup` in cache-cleanup.ts.
    /// Deletes objects (and their `.meta.json` sidecars) whose mtime is
    /// older than `max_age`. The `audit/` prefix is always skipped.
    /// Returns `(deleted_count, bytes_freed)`.
    pub async fn sweep(&self, max_age: Duration) -> Result<(u64, u64)> {
        let cutoff = SystemTime::now()
            .checked_sub(max_age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut deleted = 0u64;
        let mut freed = 0u64;
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    // audit shards are append-only history — never swept.
                    if dir == self.root && name == "audit" {
                        continue;
                    }
                    stack.push(path);
                    continue;
                }
                if name.ends_with(".meta.json") {
                    continue; // sidecars die with their object
                }
                let modified = match entry.metadata().await.and_then(|m| m.modified()) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if modified >= cutoff {
                    continue;
                }
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                if tokio::fs::remove_file(&path).await.is_ok() {
                    deleted += 1;
                    freed += size;
                    // drop the mime sidecar too, if present
                    let _ = tokio::fs::remove_file(self.sidecar_of(&path)).await;
                }
            }
        }
        Ok((deleted, freed))
    }

    /// Sidecar path for an arbitrary object path (sweep works on Paths,
    /// not keys, so it can't go through `path()`/`sidecar()`).
    fn sidecar_of(&self, object_path: &std::path::Path) -> PathBuf {
        let mut s = object_path.as_os_str().to_os_string();
        s.push(".meta.json");
        PathBuf::from(s)
    }
}

/// Shared key guard for every backend — R2-style keys may contain `/`
/// (the `audit/` prefix); `..`, `\` and absolute paths are rejected.
pub(crate) fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.contains("..") || key.contains('\\') || key.starts_with('/') {
        return Err(AppError::Internal(format!("unsafe cache key: {key:?}")));
    }
    Ok(())
}

/// Object-safe seek+read bound — a trait object can't name two principal
/// traits (`dyn AsyncRead + AsyncSeek` is E0225), so callers get this
/// single trait covering both. Blanket-implemented for every
/// `AsyncRead + AsyncSeek` type (fs `File`, `Cursor<Vec<u8>>`, ...).
pub trait AsyncReadSeek: AsyncRead + AsyncSeek {}
impl<T: AsyncRead + AsyncSeek + ?Sized> AsyncReadSeek for T {}

/// An open cached object — seek + `take()` for Range, stream for 200.
/// `stream` is a filesystem `File` on `FsCache` and an in-memory
/// `Cursor` on `S3Cache` (objects are Bot-API-capped ~20 MB).
pub struct CacheObject {
    pub stream: Pin<Box<dyn AsyncReadSeek + Send>>,
    pub size: u64,
    pub mime: Option<String>,
}

/// Streaming write handle from `begin_put` — fs writes land in a `.part`
/// file renamed on `finish`; object-store writes buffer and PUT once.
pub struct CacheWriter {
    inner: CacheWriterInner,
    bytes: u64,
}

enum CacheWriterInner {
    Fs {
        file: tokio::fs::File,
        part_path: PathBuf,
        final_path: PathBuf,
        sidecar_path: PathBuf,
    },
    S3 {
        cache: S3Cache,
        key: String,
        buf: Vec<u8>,
    },
}

impl CacheWriter {
    pub(crate) fn for_s3(cache: S3Cache, key: String) -> Self {
        Self {
            inner: CacheWriterInner::S3 {
                cache,
                key,
                buf: Vec::new(),
            },
            bytes: 0,
        }
    }

    /// Bytes written so far — for logging/telemetry.
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// Append a chunk (from the upstream stream).
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        match &mut self.inner {
            CacheWriterInner::Fs { file, .. } => file.write_all(chunk).await?,
            CacheWriterInner::S3 { buf, .. } => buf.extend_from_slice(chunk),
        }
        self.bytes += chunk.len() as u64;
        Ok(())
    }

    /// Commit the object — fs: flush + rename `.part` → final + mime
    /// sidecar; s3: one PUT with the `Content-Type` attribute.
    /// Returns total bytes committed.
    pub async fn finish(self, mime: Option<&str>) -> Result<u64> {
        let bytes = self.bytes;
        match self.inner {
            CacheWriterInner::Fs {
                mut file,
                part_path,
                final_path,
                sidecar_path,
            } => {
                file.flush().await?;
                drop(file);
                tokio::fs::rename(&part_path, &final_path).await?;
                if let Some(mime) = mime {
                    let sidecar = serde_json::json!({ "mime": mime }).to_string();
                    tokio::fs::write(&sidecar_path, sidecar).await?;
                }
            }
            CacheWriterInner::S3 { cache, key, buf } => {
                cache.put(&key, &buf, mime).await?;
            }
        }
        Ok(bytes)
    }

    /// Abandon the write — fs removes the `.part` file; s3 drops the
    /// buffer (nothing was ever visible).
    pub async fn abort(self) {
        if let CacheWriterInner::Fs { part_path, .. } = self.inner {
            let _ = tokio::fs::remove_file(&part_path).await;
        }
    }
}

/// Cache backend selected by `MEDIA_CACHE_BACKEND` — the two halves of
/// `storage.ts`: `fs` (`data/cache/`, local dev + bot scripts) and `s3`
/// (the `MEDIA_CACHE` R2 bucket). Call sites hold this enum and never
/// branch on backend.
#[derive(Clone, Debug)]
pub enum CacheBackend {
    Fs(FsCache),
    S3(S3Cache),
}

impl CacheBackend {
    /// Pick the backend from `MEDIA_CACHE_BACKEND` (`fs` default, `s3`/`r2`
    /// for object storage). `s3` fails fast when `R2_*` config is
    /// incomplete — see `S3Cache::from_config`.
    pub fn from_config(config: &Config) -> Result<Self> {
        match config.media_cache_backend.as_str() {
            "s3" | "r2" => Ok(Self::S3(S3Cache::from_config(config)?)),
            "fs" | "" => Ok(Self::Fs(FsCache::new(&config.media_cache_dir))),
            other => Err(AppError::Config(format!(
                "unknown MEDIA_CACHE_BACKEND {other:?} (expected \"fs\" or \"s3\")"
            ))),
        }
    }

    pub async fn head(&self, key: &str) -> Result<Option<CacheHead>> {
        match self {
            Self::Fs(c) => c.head(key).await,
            Self::S3(c) => c.head(key).await,
        }
    }

    pub async fn put(&self, key: &str, data: &[u8], mime: Option<&str>) -> Result<()> {
        match self {
            Self::Fs(c) => c.put(key, data, mime).await,
            Self::S3(c) => c.put(key, data, mime).await,
        }
    }

    pub async fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self {
            Self::Fs(c) => c.read(key).await,
            Self::S3(c) => c.read(key).await,
        }
    }

    pub async fn open(&self, key: &str) -> Result<Option<CacheObject>> {
        match self {
            Self::Fs(c) => c.open(key).await,
            Self::S3(c) => c.open(key).await,
        }
    }

    pub async fn begin_put(&self, key: &str) -> Result<CacheWriter> {
        match self {
            Self::Fs(c) => c.begin_put(key).await,
            Self::S3(c) => c.begin_put(key).await,
        }
    }

    pub async fn sweep(&self, max_age: Duration) -> Result<(u64, u64)> {
        match self {
            Self::Fs(c) => c.sweep(max_age).await,
            Self::S3(c) => c.sweep(max_age).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_sha1_hex_plus_ext() {
        assert_eq!(
            cache_key("abc", ".jpg"),
            "a9993e364706816aba3e25717850c26c9cd0d89d.jpg"
        );
        assert_eq!(
            cache_key("", ""),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
    }

    #[test]
    fn cache_key_rejects_traversal() {
        // head/put go through path(); exercise the guard directly.
        let cache = FsCache::new("/tmp/x");
        assert!(cache.path("../evil").is_err());
        assert!(cache.path("a/../b").is_err());
        assert!(cache.path("a\\b").is_err());
        assert!(cache.path("/abs").is_err());
        assert!(cache.path("deadbeef.jpg").is_ok());
        assert!(cache.path("audit/2025-01.jsonl").is_ok());
    }

    #[tokio::test]
    async fn open_streams_object_with_meta() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path());
        cache
            .put("abc.jpg", b"hello", Some("image/jpeg"))
            .await
            .unwrap();

        let obj = cache.open("abc.jpg").await.unwrap().unwrap();
        assert_eq!(obj.size, 5);
        assert_eq!(obj.mime.as_deref(), Some("image/jpeg"));
        assert!(cache.open("missing.bin").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn streamed_put_finishes_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path());

        let mut w = cache.begin_put("vid.mp4").await.unwrap();
        w.write_chunk(b"chunk1-").await.unwrap();
        w.write_chunk(b"chunk2").await.unwrap();
        let n = w.finish(Some("video/mp4")).await.unwrap();
        assert_eq!(n, 13);

        // .part is gone, final object + sidecar exist
        assert!(!dir.path().join("vid.mp4.part").exists());
        assert_eq!(
            cache.read("vid.mp4").await.unwrap().unwrap(),
            b"chunk1-chunk2"
        );
        let head = cache.head("vid.mp4").await.unwrap().unwrap();
        assert_eq!(head.mime.as_deref(), Some("video/mp4"));
    }

    #[tokio::test]
    async fn aborted_put_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path());
        let mut w = cache.begin_put("dead.bin").await.unwrap();
        w.write_chunk(b"partial").await.unwrap();
        w.abort().await;
        assert!(cache.head("dead.bin").await.unwrap().is_none());
        assert!(!dir.path().join("dead.bin.part").exists());
    }

    #[tokio::test]
    async fn sweep_evicts_old_objects_but_keeps_audit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = FsCache::new(dir.path());
        cache
            .put("old.jpg", b"x", Some("image/jpeg"))
            .await
            .unwrap();
        cache
            .put("audit/2025-01.jsonl", b"{}\n", None)
            .await
            .unwrap();

        // max_age 0 → everything is "expired"; audit/ must survive anyway.
        let (deleted, freed) = cache.sweep(Duration::ZERO).await.unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(freed, 1);
        assert!(cache.head("old.jpg").await.unwrap().is_none());
        assert!(dir.path().join("audit/2025-01.jsonl").exists());
        // sidecar went with the object
        assert!(!dir.path().join("old.jpg.meta.json").exists());

        // huge max_age → nothing is old enough
        cache.put("new.jpg", b"y", None).await.unwrap();
        let (d2, _) = cache.sweep(Duration::from_secs(3600)).await.unwrap();
        assert_eq!(d2, 0);
        assert!(cache.head("new.jpg").await.unwrap().is_some());
    }
}

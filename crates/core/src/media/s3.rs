//! S3-compatible object-store cache — the `MEDIA_CACHE` R2 half of
//! `storage.ts`/`audit.ts`/`cache-cleanup.ts`. Selected with
//! `MEDIA_CACHE_BACKEND=s3`; credentials come from `R2_*` env vars.
//!
//! Semantics mirror `FsCache` with two object-store adaptations:
//!
//! - MIME rides on the object's `Content-Type` attribute (the
//!   `httpMetadata.contentType` counterpart — R2 stores it per-object, so
//!   no `.meta.json` sidecars exist). It is readable on `get`/`open` via
//!   `GetResult::attributes`; `head` (a bare `HEAD`) cannot see it through
//!   `object_store`'s `ObjectMeta`, so `CacheHead::mime` is `None` there.
//!   No caller consumes `head().mime` — `open()` is the mime-bearing path.
//! - `begin_put` buffers chunks and issues a single `put` on `finish`.
//!   Every cached object is Bot-API-capped at ~20 MB, so the buffer is
//!   bounded; `abort` simply drops it (nothing was ever visible).

use std::io::Cursor;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::{AsyncReadSeek, CacheHead, CacheObject, CacheWriter, validate_key};
use crate::config::Config;
use crate::error::{AppError, Result};
use futures_util::StreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path;
use object_store::{Attribute, ObjectStore, ObjectStoreExt, PutOptions, PutPayload};

/// S3/R2-backed blob cache. `Arc<dyn ObjectStore>` keeps the concrete
/// client out of the type so tests can inject `LocalFileSystem`.
#[derive(Clone, Debug)]
pub struct S3Cache {
    store: Arc<dyn ObjectStore>,
}

impl S3Cache {
    /// Wrap any `ObjectStore` — production passes `AmazonS3`, tests pass
    /// `LocalFileSystem`.
    pub fn new(store: Arc<dyn ObjectStore>) -> Self {
        Self { store }
    }

    /// Build the R2 client from `Config` — fails fast with
    /// `AppError::Config` when `MEDIA_CACHE_BACKEND=s3` but any `R2_*`
    /// variable is missing (a silent fs fallback would hide a prod
    /// misconfiguration).
    pub fn from_config(config: &Config) -> Result<Self> {
        let missing =
            |name: &str| AppError::Config(format!("MEDIA_CACHE_BACKEND=s3 requires {name}"));
        let account_id = config
            .r2_account_id
            .clone()
            .ok_or_else(|| missing("R2_ACCOUNT_ID"))?;
        let access_key_id = config
            .r2_access_key_id
            .clone()
            .ok_or_else(|| missing("R2_ACCESS_KEY_ID"))?;
        let secret_access_key = config
            .r2_secret_access_key
            .clone()
            .ok_or_else(|| missing("R2_SECRET_ACCESS_KEY"))?;
        let bucket = config
            .r2_bucket
            .clone()
            .ok_or_else(|| missing("R2_BUCKET"))?;
        // R2's S3 endpoint; `R2_ENDPOINT` overrides for other S3-compatible
        // stores (MinIO, etc.). Path-style: R2 documents
        // `https://<account>.r2.cloudflarestorage.com/<bucket>/<key>`.
        let endpoint = config
            .r2_endpoint
            .clone()
            .unwrap_or_else(|| format!("https://{account_id}.r2.cloudflarestorage.com"));

        let store = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_endpoint(endpoint)
            .with_access_key_id(access_key_id)
            .with_secret_access_key(secret_access_key)
            // R2 accepts any region string; "auto" is its convention.
            .with_region("auto")
            .with_virtual_hosted_style_request(false)
            .build()
            .map_err(|e| AppError::Config(format!("R2 client init failed: {e}")))?;
        Ok(Self::new(Arc::new(store)))
    }

    fn path(key: &str) -> Result<Path> {
        validate_key(key)?;
        Ok(Path::from(key))
    }

    /// `cacheHead` — object size via `HEAD`. `mime` is `None`: S3 HEAD
    /// responses don't surface through `object_store`'s `ObjectMeta`.
    /// Callers that need MIME use `open()` (a GET, which returns
    /// attributes).
    pub async fn head(&self, key: &str) -> Result<Option<CacheHead>> {
        match self.store.head(&Self::path(key)?).await {
            Ok(meta) => Ok(Some(CacheHead {
                size: meta.size,
                mime: None,
            })),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(AppError::Internal(format!("cache head failed: {e}"))),
        }
    }

    /// `cachePut` — single PUT with `Content-Type` attribute when a MIME
    /// is known (the `httpMetadata` counterpart in storage.ts).
    pub async fn put(&self, key: &str, data: &[u8], mime: Option<&str>) -> Result<()> {
        let mut opts = PutOptions::default();
        if let Some(mime) = mime {
            opts.attributes
                .insert(Attribute::ContentType, mime.to_string().into());
        }
        self.store
            .put_opts(&Self::path(key)?, PutPayload::from(data.to_vec()), opts)
            .await
            .map_err(|e| AppError::Internal(format!("cache put failed: {e}")))?;
        Ok(())
    }

    /// Whole-object read — `None` when absent. Used by the audit
    /// read-concat-write append path.
    pub async fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self.store.get(&Self::path(key)?).await {
            Ok(res) => Ok(Some(
                res.bytes()
                    .await
                    .map_err(|e| AppError::Internal(format!("cache read failed: {e}")))?
                    .to_vec(),
            )),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(AppError::Internal(format!("cache get failed: {e}"))),
        }
    }

    /// `cacheGet` streaming form — one GET into memory (objects are
    /// Bot-API-capped ~20 MB), exposed as a seekable `Cursor` so Range
    /// handling is identical to the fs path.
    pub async fn open(&self, key: &str) -> Result<Option<CacheObject>> {
        let res = match self.store.get(&Self::path(key)?).await {
            Ok(res) => res,
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(AppError::Internal(format!("cache get failed: {e}"))),
        };
        let size = res.meta.size;
        let mime = res
            .attributes
            .get(&Attribute::ContentType)
            .map(|v| v.to_string());
        let bytes = res
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("cache read failed: {e}")))?;
        Ok(Some(CacheObject {
            stream: Box::pin(Cursor::new(bytes.to_vec())) as Pin<Box<dyn AsyncReadSeek + Send>>,
            size,
            mime,
        }))
    }

    /// Streaming write — buffered until `finish` issues one PUT (see the
    /// module doc for the size bound).
    pub async fn begin_put(&self, key: &str) -> Result<CacheWriter> {
        Self::path(key)?;
        Ok(CacheWriter::for_s3(self.clone(), key.to_string()))
    }

    /// TTL sweep — `runCacheCleanup` over `list`+`delete`. The `audit/`
    /// prefix is always skipped; freshness uses `last_modified` (R2's
    /// `uploaded`), matching the TS `get`-meta comparison.
    /// Returns `(deleted_count, bytes_freed)`.
    pub async fn sweep(&self, max_age: Duration) -> Result<(u64, u64)> {
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(i64::MAX);
        let max_age_ms = max_age.as_millis().min(i64::MAX as u128) as i64;
        let cutoff_ms = now_ms.saturating_sub(max_age_ms);

        let mut deleted = 0u64;
        let mut freed = 0u64;
        let mut stream = self.store.list(None);
        while let Some(meta) = stream.next().await {
            let meta = meta.map_err(|e| AppError::Internal(format!("cache list failed: {e}")))?;
            // audit shards are append-only history — never swept.
            if meta.location.to_string().starts_with("audit/") {
                continue;
            }
            if meta.last_modified.timestamp_millis() >= cutoff_ms {
                continue;
            }
            self.store
                .delete(&meta.location)
                .await
                .map_err(|e| AppError::Internal(format!("cache delete failed: {e}")))?;
            deleted += 1;
            freed += meta.size;
        }
        Ok((deleted, freed))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::media::{CacheBackend, cache_key};
    use async_trait::async_trait;
    use bytes::Bytes;
    use chrono::{DateTime, Utc};
    use futures_util::stream::{self, BoxStream};
    use object_store::{
        CopyOptions, GetOptions, GetRange, GetResult, GetResultPayload, ListResult,
        MultipartUpload, ObjectMeta, PutMultipartOptions, PutResult,
    };
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    /// In-memory `ObjectStore` exercising the trait surface `S3Cache`
    /// relies on (`put_opts` attributes, `get_opts`, `list`,
    /// `delete_stream`). `LocalFileSystem` can't stand in: it rejects
    /// `PutOptions::attributes`, exactly what S3 MIME storage needs.
    #[derive(Debug)]
    struct MemStore {
        objects: Arc<Mutex<HashMap<String, MemObject>>>,
    }

    #[derive(Debug)]
    struct MemObject {
        bytes: Bytes,
        attributes: object_store::Attributes,
        last_modified: DateTime<Utc>,
    }

    impl MemStore {
        /// Age every object's `last_modified` by `ms` so sweep-expiry
        /// tests are deterministic — `Utc::now()`'s millisecond
        /// granularity makes same-ms puts look "fresh" otherwise.
        fn age_all(&self, ms: i64) {
            let mut objects = self.objects.lock().unwrap();
            for obj in objects.values_mut() {
                obj.last_modified -= chrono::Duration::milliseconds(ms);
            }
        }
    }

    impl std::fmt::Display for MemStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "MemStore")
        }
    }

    #[async_trait]
    impl ObjectStore for MemStore {
        async fn put_opts(
            &self,
            location: &Path,
            payload: PutPayload,
            opts: PutOptions,
        ) -> object_store::Result<PutResult> {
            let mut buf = Vec::with_capacity(payload.content_length());
            for chunk in payload.iter() {
                buf.extend_from_slice(chunk);
            }
            self.objects
                .lock()
                .map_err(|e| object_store::Error::Generic {
                    store: "MemStore",
                    source: e.to_string().into(),
                })?
                .insert(
                    location.to_string(),
                    MemObject {
                        bytes: Bytes::from(buf),
                        attributes: opts.attributes,
                        last_modified: Utc::now(),
                    },
                );
            Ok(PutResult {
                e_tag: None,
                version: None,
                extensions: Default::default(),
            })
        }

        async fn put_multipart_opts(
            &self,
            _location: &Path,
            _opts: PutMultipartOptions,
        ) -> object_store::Result<Box<dyn MultipartUpload>> {
            Err(object_store::Error::NotImplemented {
                operation: "put_multipart_opts".into(),
                implementer: "MemStore".into(),
            })
        }

        async fn get_opts(
            &self,
            location: &Path,
            options: GetOptions,
        ) -> object_store::Result<GetResult> {
            let key = location.to_string();
            let objects = self
                .objects
                .lock()
                .map_err(|e| object_store::Error::Generic {
                    store: "MemStore",
                    source: e.to_string().into(),
                })?;
            let obj = objects
                .get(&key)
                .ok_or_else(|| object_store::Error::NotFound {
                    path: key.clone(),
                    source: "not found".into(),
                })?;
            let total = obj.bytes.len() as u64;
            let (start, end) = match &options.range {
                Some(GetRange::Bounded(r)) => (r.start, r.end.min(total)),
                Some(GetRange::Offset(o)) => (*o, total),
                Some(GetRange::Suffix(n)) => (total.saturating_sub(*n), total),
                None => (0, total),
            };
            let meta = ObjectMeta {
                location: location.clone(),
                last_modified: obj.last_modified,
                size: total,
                e_tag: None,
                version: None,
            };
            let slice = if options.head {
                Bytes::new()
            } else {
                obj.bytes.slice(start as usize..end as usize)
            };
            Ok(GetResult {
                payload: GetResultPayload::Stream(stream::once(async move { Ok(slice) }).boxed()),
                meta,
                range: start..end,
                attributes: obj.attributes.clone(),
                extensions: Default::default(),
            })
        }

        fn delete_stream(
            &self,
            locations: BoxStream<'static, object_store::Result<Path>>,
        ) -> BoxStream<'static, object_store::Result<Path>> {
            let objects = Arc::clone(&self.objects);
            locations
                .map(move |loc| {
                    loc.inspect(|path| {
                        let _ = objects.lock().map(|mut m| {
                            m.remove(&path.to_string());
                        });
                    })
                })
                .boxed()
        }

        fn list(
            &self,
            prefix: Option<&Path>,
        ) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
            let pfx = prefix.map(|p| p.to_string()).unwrap_or_default();
            let metas: Vec<object_store::Result<ObjectMeta>> = self
                .objects
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k.starts_with(&pfx))
                .map(|(k, o)| {
                    Ok(ObjectMeta {
                        location: Path::from(k.as_str()),
                        last_modified: o.last_modified,
                        size: o.bytes.len() as u64,
                        e_tag: None,
                        version: None,
                    })
                })
                .collect();
            stream::iter(metas).boxed()
        }

        async fn list_with_delimiter(
            &self,
            prefix: Option<&Path>,
        ) -> object_store::Result<ListResult> {
            let mut metas = Vec::new();
            let mut stream = self.list(prefix);
            while let Some(meta) = stream.next().await {
                metas.push(meta?);
            }
            Ok(ListResult {
                common_prefixes: vec![],
                objects: metas,
                extensions: Default::default(),
            })
        }

        async fn copy_opts(
            &self,
            _from: &Path,
            _to: &Path,
            _options: CopyOptions,
        ) -> object_store::Result<()> {
            Err(object_store::Error::NotImplemented {
                operation: "copy_opts".into(),
                implementer: "MemStore".into(),
            })
        }
    }

    fn test_cache() -> (S3Cache, Arc<MemStore>) {
        let store = Arc::new(MemStore {
            objects: Arc::new(Mutex::new(HashMap::new())),
        });
        (S3Cache::new(store.clone() as Arc<dyn ObjectStore>), store)
    }

    #[tokio::test]
    async fn put_head_read_roundtrip() {
        let (cache, _store) = test_cache();
        assert!(cache.head("missing.bin").await.unwrap().is_none());

        cache
            .put("abc.jpg", b"pixels", Some("image/jpeg"))
            .await
            .unwrap();

        let head = cache.head("abc.jpg").await.unwrap().unwrap();
        assert_eq!(head.size, 6);
        // HEAD can't surface Content-Type through ObjectMeta.
        assert!(head.mime.is_none());

        assert_eq!(cache.read("abc.jpg").await.unwrap().unwrap(), b"pixels");
        assert!(cache.read("missing.bin").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn open_streams_seekable_object_with_mime() {
        let (cache, _store) = test_cache();
        cache
            .put("vid.mp4", b"0123456789", Some("video/mp4"))
            .await
            .unwrap();

        let mut obj = cache.open("vid.mp4").await.unwrap().unwrap();
        assert_eq!(obj.size, 10);
        assert_eq!(obj.mime.as_deref(), Some("video/mp4"));

        // Range read: seek to 4, take 3 → "456".
        obj.stream.seek(std::io::SeekFrom::Start(4)).await.unwrap();
        let mut buf = Vec::new();
        obj.stream.take(3).read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"456");

        assert!(cache.open("missing.bin").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn streamed_put_commits_on_finish() {
        let (cache, _store) = test_cache();
        let mut w = cache.begin_put("clip.mp4").await.unwrap();
        w.write_chunk(b"chunk1-").await.unwrap();
        w.write_chunk(b"chunk2").await.unwrap();
        let n = w.finish(Some("video/mp4")).await.unwrap();
        assert_eq!(n, 13);

        assert_eq!(
            cache.read("clip.mp4").await.unwrap().unwrap(),
            b"chunk1-chunk2"
        );
        let obj = cache.open("clip.mp4").await.unwrap().unwrap();
        assert_eq!(obj.mime.as_deref(), Some("video/mp4"));
    }

    #[tokio::test]
    async fn aborted_put_leaves_nothing() {
        let (cache, _store) = test_cache();
        let mut w = cache.begin_put("dead.bin").await.unwrap();
        w.write_chunk(b"partial").await.unwrap();
        w.abort().await;
        assert!(cache.head("dead.bin").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn sweep_evicts_old_objects_but_keeps_audit() {
        let (cache, store) = test_cache();
        cache.put("old.jpg", b"x", None).await.unwrap();
        cache
            .put("audit/2025-01.jsonl", b"{}\n", None)
            .await
            .unwrap();
        store.age_all(10_000);

        // 10s-old + max_age 0 → expired; audit/ survives anyway.
        let (deleted, freed) = cache.sweep(Duration::ZERO).await.unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(freed, 1);
        assert!(cache.head("old.jpg").await.unwrap().is_none());
        assert!(cache.read("audit/2025-01.jsonl").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn sweep_keeps_fresh_objects() {
        let (cache, store) = test_cache();
        cache.put("old.jpg", b"x", None).await.unwrap();
        cache.put("new.jpg", b"y", None).await.unwrap();
        store.age_all(120_000);
        cache.put("fresh.jpg", b"z", None).await.unwrap();

        // Only the two 120s-old objects are deletable at max_age 60s.
        let (deleted, _) = cache.sweep(Duration::from_secs(60)).await.unwrap();
        assert_eq!(deleted, 2);
        assert!(cache.head("fresh.jpg").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn unsafe_keys_are_rejected() {
        let (cache, _store) = test_cache();
        for bad in ["", "../evil", "a/../b", "a\\b", "/abs"] {
            assert!(cache.head(bad).await.is_err(), "key {bad:?} accepted");
        }
        assert!(cache.head("audit/2025-01.jsonl").await.is_ok());
        // cache_key output is always a safe sha1 hex + ext.
        let k = cache_key("file-id", ".jpg");
        assert!(cache.head(&k).await.unwrap().is_none());
    }

    #[test]
    fn backend_defaults_to_fs_and_rejects_unknown() {
        let mut config = Config::from_env().unwrap();
        config.media_cache_backend = "bogus".to_string();
        assert!(CacheBackend::from_config(&config).is_err());

        config.media_cache_backend = "fs".to_string();
        assert!(matches!(
            CacheBackend::from_config(&config).unwrap(),
            CacheBackend::Fs(_)
        ));
    }

    #[test]
    fn s3_backend_fails_fast_on_missing_r2_config() {
        let mut config = Config::from_env().unwrap();
        config.media_cache_backend = "s3".to_string();
        config.r2_account_id = None;
        config.r2_access_key_id = None;
        config.r2_secret_access_key = None;
        config.r2_bucket = None;
        config.r2_endpoint = None;
        let err = CacheBackend::from_config(&config).unwrap_err();
        assert!(err.to_string().contains("R2_ACCOUNT_ID"));
    }
}

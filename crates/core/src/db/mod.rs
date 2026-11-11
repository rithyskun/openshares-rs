//! Data access layer — port of `src/db/index.ts`.
//!
//! The TS version wraps `@libsql/client` (SQLite over HTTP to Turso). Here
//! we use the `libsql` crate's `Builder::new_remote(url, auth_token)` —
//! remote Turso only, same as the TS app's `createClient`. Repositories
//! take `&Db` and use the `fetch_*`/`execute` helpers below.
//!
//! Local SQLite (`Builder::new_local`) exists ONLY as the unit-test
//! harness — `Db::connect` is compiled under `cfg(test)` and the
//! `testing` feature (pulled in by dependent crates' dev-deps), so no
//! shipped binary can talk to a local db.
//!
//! `Db::migrate` tracks applied files in `schema_migrations
//! (name TEXT PRIMARY KEY, applied_at INTEGER)` — the same table and row
//! format `scripts/setup-db.ts` wrote, so a Turso database it already
//! migrated needs no re-application.

pub mod repos;

use std::collections::HashSet;
#[cfg(any(test, feature = "testing"))]
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use libsql::{Builder, Connection, Database, Value};
use serde::de::DeserializeOwned;
use tokio::time::timeout;

use crate::error::{AppError, Result};

/// Public share-link lifetime — mirrors `PUBLIC_LINK_TTL` in db/index.ts.
pub const PUBLIC_LINK_TTL: i64 = 7 * 24 * 60 * 60;

/// Bound on a single remote round trip. Turso queries run in ~100s of ms;
/// 10s only ever bites when a pooled connection is dead-but-not-closed
/// (NAT drop, edge GOAWAY) and would otherwise stall the request until
/// TCP gives up. Local queries never approach it.
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Remote connection-pool width. libsql serializes every round trip
/// through one `Mutex<RawStream>` per `Connection` — a single shared
/// conn makes ALL concurrent requests queue behind each other's HTTP
/// RTTs (an SSR page fanning out fetches pays the sum, not the max).
/// Each slot is an independent hrana stream, so pool width is the real
/// query parallelism. Local keeps a single slot.
const REMOTE_POOL_SIZE: usize = 8;

/// A remote conn with no successful round trip in this window is
/// suspect — Turso's edge and client NATs reap idle conns, and the
/// keepalive only proves liveness every 20s. Older than this and the
/// conn is probed before a real query parks on it.
const STALE_AFTER: Duration = Duration::from_secs(45);

/// Bound on the `SELECT 1` liveness probe — one RTT on a live conn.
/// Tight on purpose: a wedged conn gets reconnected here instead of a
/// request eating the full `QUERY_TIMEOUT`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// `(generation, conn, last-ok)` slot — the conn is swappable because a
/// remote hrana *stream* dies permanently when Turso reaps its HTTP
/// conn: every later query 404s `stream not found` until
/// `Database::connect()` mints a new `Connection`. The generation
/// dedupes concurrent reconnects on the same slot. `last_ok` stamps
/// the last proven round trip and drives the staleness probe.
struct Slot {
    rev: u64,
    conn: Connection,
    last_ok: Instant,
}

type ConnSlot = Arc<Mutex<Slot>>;

/// Read statements are idempotent — safe to replay on a fresh conn after
/// a transport error. Writes (`INSERT .. RETURNING` included) are not:
/// "connection closed before message completed" can mean the write
/// landed but the response was lost.
fn is_read(sql: &str) -> bool {
    let kw: String = sql
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_uppercase();
    matches!(
        kw.as_str(),
        "SELECT" | "WITH" | "PRAGMA" | "EXPLAIN" | "VALUES"
    )
}

/// The remote hrana *stream* (server-side state tied to the `Connection`)
/// is gone — sqld 404s every later query with `stream not found`. The
/// request provably never executed, so retrying on a fresh `Connection`
/// is safe for reads and writes alike.
fn is_stream_dead(e: &libsql::Error) -> bool {
    match e {
        libsql::Error::Hrana(inner) => {
            let m = inner.to_string();
            m.contains("stream")
                && (m.contains("not found")
                    || m.contains("closed")
                    || m.contains("expired")
                    || m.contains("gone")
                    || m.contains("does not exist"))
        }
        _ => false,
    }
}

/// Transport-level failures where the request definitely never produced
/// a server-side result we could observe — a stale pooled conn. SQL
/// errors (`SqliteFailure`, `RemoteSqliteFailure`) are answers from the
/// server and must surface, not retry.
fn is_transient(e: &libsql::Error) -> bool {
    match e {
        libsql::Error::ConnectionFailed(_) => true,
        libsql::Error::Hrana(inner) => {
            let m = inner.to_string();
            [
                "connection closed",
                "incomplete message",
                "send request",
                "trying to connect",
                "channel closed",
                "connection reset",
                "broken pipe",
                "connection refused",
                "dns error",
                "timed out",
            ]
            .iter()
            .any(|p| m.contains(p))
        }
        _ => false,
    }
}

/// Unix seconds — mirrors `now()` in db/index.ts.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Shared database handle. Cheap to clone — wraps the libsql pool.
#[derive(Clone)]
pub struct Db {
    /// Connection slots. The mutex guards only the `(rev, conn)` swap —
    /// it is held for the `clone()` in [`Db::checkout`], never across a
    /// query, so waiting on a slot costs ~ns while the slot's *internal*
    /// stream mutex does the real serialization.
    slots: Arc<[ConnSlot]>,
    /// Round-robin checkout cursor — shared across `Db` clones.
    cursor: Arc<AtomicUsize>,
    /// `Connection` does not keep the `Database` open on its own — hold
    /// it. Also the reconnect source.
    database: Arc<Database>,
    /// Remote backend — drives the keepalive and gives `transaction()`
    /// a dedicated conn (a pooled slot's shared stream would let other
    /// statements join the tx's baton).
    remote: bool,
}

impl Db {
    /// TEST HARNESS — connect to a local SQLite file, creating it if
    /// needed. Accepts `sqlite://<path>`, `sqlite:<path>`
    /// (`sqlite::memory:` works) or a bare path. Compiled only for test
    /// builds / the `testing` feature: shipped code is Turso-only like
    /// the TS app's `createClient({url, authToken})`. Does NOT run
    /// migrations — call [`Db::migrate`] explicitly (same convention as
    /// `npm run db:setup`, which never migrates at runtime).
    #[cfg(any(test, feature = "testing"))]
    pub async fn connect(url: &str) -> Result<Self> {
        let path = local_path(url);

        // SQLite won't create missing parent directories for us.
        if path != ":memory:"
            && let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let database = Builder::new_local(path).build().await?;
        let conn = database.connect()?;
        let _ = conn
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000")
            .await;

        Ok(Self {
            slots: Arc::from([Self::slot(conn)]),
            cursor: Arc::new(AtomicUsize::new(0)),
            database: Arc::new(database),
            remote: false,
        })
    }

    /// Connect to remote Turso/libSQL — `TURSO_DB_URL` (`libsql://` or
    /// `https://`) + `TURSO_AUTH_TOKEN`. Mirrors `createClient({url,
    /// authToken})` in the TS `db/index.ts`.
    pub async fn connect_remote(url: &str, auth_token: &str) -> Result<Self> {
        let database = Arc::new(
            Builder::new_remote(url.to_string(), auth_token.to_string())
                .build()
                .await?,
        );

        // `connect()` is wire-free (the hrana stream materializes on first
        // use); only the per-conn `PRAGMA` costs a round trip, so the slots
        // are primed in parallel rather than paying N sequential RTTs.
        let mut join = tokio::task::JoinSet::new();
        for _ in 0..REMOTE_POOL_SIZE {
            let database = database.clone();
            join.spawn(async move {
                let conn = database.connect()?;
                let _ = conn.execute_batch("PRAGMA foreign_keys = ON").await;
                Ok::<_, libsql::Error>(conn)
            });
        }
        let mut slots = Vec::with_capacity(REMOTE_POOL_SIZE);
        while let Some(res) = join.join_next().await {
            let conn = res.map_err(|_| AppError::Internal("db pool task panicked".into()))??;
            slots.push(Self::slot(conn));
        }

        let db = Self {
            slots: Arc::from(slots),
            cursor: Arc::new(AtomicUsize::new(0)),
            database,
            remote: true,
        };

        // Turso's edge reaps idle HTTP connections, and a dead pooled conn
        // parks or fails the next remote query. A bounded `SELECT 1` per
        // slot keeps every stream warm — and probing all slots in parallel
        // heals a mass kill (laptop sleep, NAT drop) in one probe window
        // instead of N serial `QUERY_TIMEOUT` stalls.
        let ping = db.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(20)).await;
                let mut join = tokio::task::JoinSet::new();
                for slot in ping.slots.iter() {
                    let slot = slot.clone();
                    let ping = ping.clone();
                    join.spawn(async move {
                        let (rev, conn) = {
                            let g = slot.lock().unwrap_or_else(|e| e.into_inner());
                            (g.rev, g.conn.clone())
                        };
                        ping.probe(&slot, rev, conn).await;
                    });
                }
                while join.join_next().await.is_some() {}
            }
        });

        Ok(db)
    }

    /// Whether this `Db` talks to remote Turso (hrana) rather than a
    /// test-harness local SQLite file — the stale-probe and
    /// `transaction()` conn routing gate on it.
    pub fn is_remote(&self) -> bool {
        self.remote
    }

    /// Connect to remote Turso from config — `TURSO_DB_URL` is required
    /// (the TS app had no local fallback either). `TURSO_AUTH_TOKEN` may
    /// be absent for unauthenticated local sqld.
    pub async fn connect_from(config: &crate::config::Config) -> Result<Self> {
        let url = config
            .turso_db_url
            .as_deref()
            .ok_or_else(|| AppError::Config("TURSO_DB_URL is required".into()))?;
        tracing::info!(db = "turso", "connecting to remote libsql database");
        Self::connect_remote(url, config.turso_auth_token.as_deref().unwrap_or("")).await
    }

    /// Apply pending migrations from `crates/core/migrations/` — tracked in
    /// `schema_migrations`, the same bookkeeping table `setup-db.ts` used.
    pub async fn migrate(&self) -> Result<()> {
        self.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                    name TEXT PRIMARY KEY, applied_at INTEGER NOT NULL
                 )",
        )
        .await?;

        let applied: HashSet<String> = self
            .query_rows("SELECT name FROM schema_migrations", ())
            .await?
            .iter()
            .filter_map(|r| r.get::<String>(0).ok())
            .collect();

        if applied.is_empty() && self.table_exists("users").await? {
            // Schema predates this runner — applied by `sqlx::migrate` in
            // local dev or by hand. Record everything rather than re-running
            // CREATE TABLEs that would fail on "already exists".
            for (name, _) in MIGRATIONS {
                self.execute(
                    "INSERT OR IGNORE INTO schema_migrations (name, applied_at) VALUES (?, ?)",
                    libsql::params![*name, now()],
                )
                .await?;
            }
            return Ok(());
        }

        for (name, sql) in MIGRATIONS {
            if applied.contains(*name) {
                continue;
            }
            self.execute_batch(sql).await?;
            self.execute(
                "INSERT INTO schema_migrations (name, applied_at) VALUES (?, ?)",
                libsql::params![*name, now()],
            )
            .await?;
        }
        Ok(())
    }

    async fn table_exists(&self, name: &str) -> Result<bool> {
        Ok(self
            .scalar_opt::<i64>(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
                libsql::params![name],
            )
            .await?
            .is_some())
    }

    /* ------------------- query helpers (sqlx verbs) ------------------- */

    fn slot(conn: Connection) -> ConnSlot {
        Arc::new(Mutex::new(Slot {
            rev: 0,
            conn,
            last_ok: Instant::now(),
        }))
    }

    /// Round-robin slot checkout — returns the slot handle, its current
    /// generation and a cloned `Connection` (the clone shares the slot's
    /// internal stream, so parallel queries spread across *slots*).
    fn checkout(&self) -> (ConnSlot, u64, Connection) {
        let idx = self.cursor.fetch_add(1, Ordering::Relaxed) % self.slots.len();
        let slot = self.slots[idx].clone();
        let (rev, conn) = {
            let g = slot.lock().unwrap_or_else(|e| e.into_inner());
            (g.rev, g.conn.clone())
        };
        (slot, rev, conn)
    }

    /// Swap a slot's `Connection` for a fresh one — the only cure for a
    /// dead hrana stream. If another task already reconnected this slot
    /// (`rev` moved on), take their conn instead of dialing a second one.
    fn reconnect(database: &Database, slot: &ConnSlot, rev: u64) -> Result<(u64, Connection)> {
        let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
        if g.rev == rev {
            g.conn = database.connect()?;
            g.rev += 1;
            g.last_ok = Instant::now();
        }
        Ok((g.rev, g.conn.clone()))
    }

    /// Slot proven by a successful round trip — resets the staleness
    /// window so live conns never pay a probe.
    fn touch(slot: &ConnSlot) {
        slot.lock().unwrap_or_else(|e| e.into_inner()).last_ok = Instant::now();
    }

    /// True when the slot's last *proven* round trip is older than
    /// [`STALE_AFTER`] — checkout callers probe before trusting it.
    fn stale(slot: &ConnSlot) -> bool {
        slot.lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_ok
            .elapsed()
            > STALE_AFTER
    }

    /// Bound `SELECT 1` on `conn`. Success marks the slot fresh; any
    /// failure — dead stream, wedged TCP, timeout — swaps in a fresh
    /// conn from `database` (keeping the old one if redial fails, so
    /// the caller's normal retry ladder still decides).
    async fn probe(&self, slot: &ConnSlot, rev: u64, conn: Connection) -> (u64, Connection) {
        match timeout(PROBE_TIMEOUT, conn.execute("SELECT 1", ())).await {
            Ok(Ok(_)) => {
                Self::touch(slot);
                (rev, conn)
            }
            _ => Self::reconnect(&self.database, slot, rev).unwrap_or((rev, conn)),
        }
    }

    /// Every remote round trip funnels through here. Failure ladder:
    ///
    /// - `stream not found`/`closed`/`expired` → the statement provably
    ///   never executed, so reconnect + retry is safe for reads *and*
    ///   writes.
    /// - transport error on a read → same-conn retry (a redialed TCP conn
    ///   may still carry the stream).
    /// - timeout on a read → the conn is likely wedged; reconnect + retry.
    /// - writes on any other error → surface it (a lost response could
    ///   mean the write landed).
    ///
    /// For remote hrana the full result set arrives in one response, so
    /// `conn.query` is the only network boundary; `Rows::next()` decodes.
    async fn q(&self, sql: &str, params: libsql::params::Params) -> Result<libsql::Rows> {
        let read = is_read(sql);
        let (slot, mut rev, mut conn) = self.checkout();
        if self.remote && Self::stale(&slot) {
            (rev, conn) = self.probe(&slot, rev, conn).await;
        }
        for _ in 0..3 {
            match timeout(QUERY_TIMEOUT, conn.query(sql, params.clone())).await {
                Ok(Ok(rows)) => {
                    Self::touch(&slot);
                    return Ok(rows);
                }
                Ok(Err(e)) if is_stream_dead(&e) => {
                    tracing::warn!(error = %e, "db: remote stream lost, reconnecting");
                    (rev, conn) = Self::reconnect(&self.database, &slot, rev)?;
                }
                Ok(Err(e)) if read && is_transient(&e) => {
                    tracing::warn!(error = %e, "db: transient error on read, retrying");
                }
                Ok(Err(e)) => return Err(AppError::Database(e)),
                Err(_) if read => {
                    tracing::warn!("db: query timed out, reconnecting");
                    (rev, conn) = Self::reconnect(&self.database, &slot, rev)?;
                }
                Err(_) => return Err(AppError::Internal("db query timed out".into())),
            }
        }
        Err(AppError::Internal("db query failed after retries".into()))
    }

    /// Guarded `conn.query` for call sites that need `Rows` itself
    /// (dynamic SQL, per-row `get`) — same timeout/retry as the helpers.
    pub async fn query_guard(
        &self,
        sql: &str,
        params: impl libsql::params::IntoParams,
    ) -> Result<libsql::Rows> {
        self.q(sql, params.into_params()?).await
    }

    /// `fetch_optional` — first row deserialized by column name into `T`
    /// (`serde::Deserialize`), or `None`. `de::from_row` runs inside the
    /// live cursor window (see [`Db::query_rows`]).
    pub async fn query_opt<T: DeserializeOwned>(
        &self,
        sql: &str,
        params: impl libsql::params::IntoParams,
    ) -> Result<Option<T>> {
        let mut rows = self.q(sql, params.into_params()?).await?;
        match rows.next().await? {
            Some(row) => Ok(Some(libsql::de::from_row(&row).map_err(de_err)?)),
            None => Ok(None),
        }
    }

    /// `fetch_one` — first row or `QueryReturnedNoRows` (→ 500, same as
    /// sqlx `RowNotFound`).
    pub async fn query_one<T: DeserializeOwned>(
        &self,
        sql: &str,
        params: impl libsql::params::IntoParams,
    ) -> Result<T> {
        self.query_opt(sql, params)
            .await?
            .ok_or_else(|| AppError::Database(libsql::Error::QueryReturnedNoRows))
    }

    /// `fetch_all` — every row deserialized into `T`. `de::from_row` runs
    /// inside the live cursor window (see [`Db::query_rows`]).
    pub async fn query_all<T: DeserializeOwned>(
        &self,
        sql: &str,
        params: impl libsql::params::IntoParams,
    ) -> Result<Vec<T>> {
        let mut rows = self.q(sql, params.into_params()?).await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(libsql::de::from_row(&row).map_err(de_err)?);
        }
        Ok(out)
    }

    /// `query_scalar` + `fetch_one` — first column of the first row; empty
    /// result → `QueryReturnedNoRows` (500 parity with sqlx `RowNotFound`).
    pub async fn scalar<T: Scalar>(
        &self,
        sql: &str,
        params: impl libsql::params::IntoParams,
    ) -> Result<T> {
        let mut rows = self.q(sql, params.into_params()?).await?;
        match rows.next().await? {
            Some(row) => T::from_value(&row.get_value(0)?),
            None => Err(AppError::Database(libsql::Error::QueryReturnedNoRows)),
        }
    }

    /// `query_scalar` + `fetch_optional`.
    pub async fn scalar_opt<T: Scalar>(
        &self,
        sql: &str,
        params: impl libsql::params::IntoParams,
    ) -> Result<Option<T>> {
        let mut rows = self.q(sql, params.into_params()?).await?;
        match rows.next().await? {
            Some(row) => Ok(Some(T::from_value(&row.get_value(0)?)?)),
            None => Ok(None),
        }
    }

    /// Raw rows — for tuple/positional results (`row.get::<T>(0)`,
    /// `row.get::<T>(1)`) where a struct would be ceremony. Values are
    /// snapshotted while the statement cursor is live: `libsql::Row`
    /// reads the cursor's *current* position, so rows collected from
    /// `Rows::next()` decode as `Null` once the cursor is done.
    pub async fn query_rows(
        &self,
        sql: &str,
        params: impl libsql::params::IntoParams,
    ) -> Result<Vec<RowSnapshot>> {
        let mut rows = self.q(sql, params.into_params()?).await?;
        let columns: Vec<String> = (0..rows.column_count())
            .filter_map(|i| rows.column_name(i).map(str::to_string))
            .collect();
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let mut values = Vec::new();
            for i in 0..row.column_count() {
                values.push(row.get_value(i)?);
            }
            out.push(RowSnapshot {
                columns: columns.clone(),
                values,
            });
        }
        Ok(out)
    }

    /// `execute` — returns rows affected (sqlx `rows_affected()`).
    /// Timeout-bounded like [`Db::q`]; retried only on dead-stream errors,
    /// where the statement provably never ran — a lost write response
    /// otherwise means it may have landed.
    pub async fn execute(&self, sql: &str, params: impl libsql::params::IntoParams) -> Result<u64> {
        let params = params.into_params()?;
        let (slot, mut rev, mut conn) = self.checkout();
        if self.remote && Self::stale(&slot) {
            (rev, conn) = self.probe(&slot, rev, conn).await;
        }
        for _ in 0..2 {
            match timeout(QUERY_TIMEOUT, conn.execute(sql, params.clone())).await {
                Ok(Ok(n)) => {
                    Self::touch(&slot);
                    return Ok(n);
                }
                Ok(Err(e)) if is_stream_dead(&e) => {
                    tracing::warn!(error = %e, "db: remote stream lost, reconnecting");
                    (rev, conn) = Self::reconnect(&self.database, &slot, rev)?;
                }
                Ok(Err(e)) => return Err(AppError::Database(e)),
                Err(_) => return Err(AppError::Internal("db execute timed out".into())),
            }
        }
        Err(AppError::Internal("db execute failed after retry".into()))
    }

    /// Multi-statement script — migrations and `PRAGMA`.
    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        let (slot, mut rev, mut conn) = self.checkout();
        if self.remote && Self::stale(&slot) {
            (rev, conn) = self.probe(&slot, rev, conn).await;
        }
        for _ in 0..2 {
            match timeout(QUERY_TIMEOUT, conn.execute_batch(sql)).await {
                Ok(Ok(_)) => {
                    Self::touch(&slot);
                    return Ok(());
                }
                Ok(Err(e)) if is_stream_dead(&e) => {
                    tracing::warn!(error = %e, "db: remote stream lost, reconnecting");
                    (rev, conn) = Self::reconnect(&self.database, &slot, rev)?;
                }
                Ok(Err(e)) => return Err(AppError::Database(e)),
                Err(_) => return Err(AppError::Internal("db batch timed out".into())),
            }
        }
        Err(AppError::Internal("db batch failed after retry".into()))
    }

    /// Interactive transaction (`tx.query`/`tx.execute`/`tx.commit`) —
    /// replaces `db.pool().begin()`. Remote txs get a *dedicated* conn:
    /// a pooled slot's stream is shared, so concurrent statements would
    /// silently join the tx's baton. Local keeps the slot conn —
    /// `:memory:` databases would be empty on a fresh conn.
    pub async fn transaction(&self) -> Result<libsql::Transaction> {
        for _ in 0..2 {
            let conn = if self.remote {
                let conn = self.database.connect()?;
                let _ = conn.execute_batch("PRAGMA foreign_keys = ON").await;
                conn
            } else {
                self.checkout().2
            };
            match timeout(QUERY_TIMEOUT, conn.transaction()).await {
                Ok(Ok(tx)) => return Ok(tx),
                Ok(Err(e)) if is_stream_dead(&e) || (self.remote && is_transient(&e)) => {
                    tracing::warn!(error = %e, "db: transaction conn lost, redialing");
                }
                Ok(Err(e)) => return Err(AppError::Database(e)),
                Err(_) => return Err(AppError::Internal("db transaction timed out".into())),
            }
        }
        Err(AppError::Internal(
            "db transaction failed after retry".into(),
        ))
    }

    /// Raw connection clone — escape hatch for call sites the helpers
    /// don't cover. Note the handle can be swapped out by a reconnect;
    /// prefer the `Db` helpers, which follow swaps automatically.
    pub fn conn(&self) -> Connection {
        self.checkout().2
    }
}

/// An owned row — `libsql::Row` is a live cursor view (see
/// [`Db::query_rows`]), so snapshots carry `Vec<Value>` + column names.
/// `get::<T>` mirrors `libsql::Row::get`'s call shape.
pub struct RowSnapshot {
    columns: Vec<String>,
    values: Vec<Value>,
}

impl RowSnapshot {
    /// Column value by index, decoded via [`Scalar`].
    pub fn get<T: Scalar>(&self, idx: i32) -> Result<T> {
        match self.values.get(idx as usize) {
            Some(v) => T::from_value(v),
            None => Err(AppError::Internal(format!(
                "column index {idx} out of range ({} columns)",
                self.values.len()
            ))),
        }
    }

    /// Column name by index (same contract as `libsql::Row::column_name`).
    pub fn column_name(&self, idx: i32) -> Option<&str> {
        self.columns.get(idx as usize).map(String::as_str)
    }
}

/// Scalar decode for [`Db::scalar`]/[`Db::scalar_opt`]/[`RowSnapshot::get`].
/// libsql's `FromValue` bound lives in a private module and can't be
/// named here, so concrete column types get their own trait instead.
pub trait Scalar: Sized {
    fn from_value(v: &Value) -> Result<Self>;
}

fn type_err(expected: &str, v: &Value) -> AppError {
    AppError::Internal(format!("db decode: expected {expected}, got {v:?}"))
}

macro_rules! impl_scalar_int {
    ($($t:ty),+) => {
        $(impl Scalar for $t {
            fn from_value(v: &Value) -> Result<Self> {
                match v {
                    Value::Integer(i) => <$t>::try_from(*i)
                        .map_err(|_| AppError::Internal(format!("db decode: {i} overflows"))),
                    _ => Err(type_err("INTEGER", v)),
                }
            }
        })+
    };
}

impl_scalar_int!(i32, i64, u32, u64);

impl Scalar for f64 {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Real(f) => Ok(*f),
            Value::Integer(i) => Ok(*i as f64),
            _ => Err(type_err("REAL", v)),
        }
    }
}

impl Scalar for String {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Text(s) => Ok(s.clone()),
            _ => Err(type_err("TEXT", v)),
        }
    }
}

impl Scalar for Vec<u8> {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Blob(b) => Ok(b.clone()),
            _ => Err(type_err("BLOB", v)),
        }
    }
}

impl Scalar for bool {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Integer(i) => Ok(*i != 0),
            _ => Err(type_err("INTEGER", v)),
        }
    }
}

/// `NULL` → `None`, anything else decodes as `T`.
impl<T: Scalar> Scalar for Option<T> {
    fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Null => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }
}

// `INSERT ... RETURNING id` is the rowid source everywhere the TS code
// used `lastInsertRowid` — preferred over `conn.last_insert_rowid()`
// because the shared `Connection` can interleave concurrent inserts.

/// Convert a `serde` row-decode failure into the `Database` variant so
/// callers keep the same 500 mapping sqlx gave us.
fn de_err(e: serde::de::value::Error) -> AppError {
    AppError::Database(libsql::Error::ToSqlConversionFailure(e.into()))
}

/// Strip the `sqlite://`/`sqlite:` scheme — test-harness `Db::connect`.
#[cfg(any(test, feature = "testing"))]
fn local_path(url: &str) -> &str {
    url.strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"))
        .unwrap_or(url)
}

/// `(filename, sql)` pairs — the names are written to `schema_migrations`
/// verbatim (`.sql` included), matching `scripts/setup-db.ts`.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_init.sql",
        include_str!("../../migrations/0001_init.sql"),
    ),
    (
        "0002_rate_limits.sql",
        include_str!("../../migrations/0002_rate_limits.sql"),
    ),
    (
        "0003_rbac.sql",
        include_str!("../../migrations/0003_rbac.sql"),
    ),
    (
        "0004_settings.sql",
        include_str!("../../migrations/0004_settings.sql"),
    ),
    (
        "0005_bot_state.sql",
        include_str!("../../migrations/0005_bot_state.sql"),
    ),
    (
        "0006_media_group.sql",
        include_str!("../../migrations/0006_media_group.sql"),
    ),
    (
        "0007_media_missing.sql",
        include_str!("../../migrations/0007_media_missing.sql"),
    ),
    (
        "0008_session_created.sql",
        include_str!("../../migrations/0008_session_created.sql"),
    ),
    (
        "0009_media_album_idx.sql",
        include_str!("../../migrations/0009_media_album_idx.sql"),
    ),
    (
        "0010_share_views_and_media_indexes.sql",
        include_str!("../../migrations/0010_share_views_and_media_indexes.sql"),
    ),
    (
        "0011_share_likes.sql",
        include_str!("../../migrations/0011_share_likes.sql"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("t.db").display()))
            .await
            .unwrap();
        (db, dir)
    }

    #[test]
    fn read_write_statement_classification() {
        assert!(is_read("SELECT 1"));
        assert!(is_read("  with x as (select 1) select * from x"));
        assert!(is_read("PRAGMA foreign_keys = ON"));
        assert!(!is_read("INSERT INTO t VALUES (1)"));
        assert!(!is_read("INSERT INTO t VALUES (1) RETURNING id"));
        assert!(!is_read("UPDATE t SET a = 1"));
        assert!(!is_read("DELETE FROM t"));
        assert!(!is_read("CREATE TABLE t (a)"));
    }

    #[tokio::test]
    async fn reconnect_swaps_conn_once_per_generation() {
        let (db, _dir) = test_db().await;
        assert_eq!(db.slots.len(), 1);
        let (slot, g0, _) = db.checkout();

        // First caller performs the swap.
        let (g1, c1) = Db::reconnect(&db.database, &slot, g0).unwrap();
        assert_eq!(g1, g0 + 1);

        // A concurrent caller that saw the old generation does NOT dial
        // again — it picks up the conn the first caller installed.
        let (g2, c2) = Db::reconnect(&db.database, &slot, g0).unwrap();
        assert_eq!(g2, g1);

        // The slot now serves the swapped conn; both clones work against
        // the same database.
        let (_, g3, _) = db.checkout();
        assert_eq!(g3, g1);
        c1.execute("CREATE TABLE swap_probe (a)", ()).await.unwrap();
        c2.query("SELECT 1 FROM sqlite_master WHERE name = 'swap_probe'", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn query_survives_reconnect() {
        let (db, _dir) = test_db().await;
        db.execute("CREATE TABLE survive (a)", ()).await.unwrap();
        let (slot, g0, _) = db.checkout();
        Db::reconnect(&db.database, &slot, g0).unwrap();
        // `q` snapshots the handle per call — post-swap queries work.
        assert_eq!(db.scalar::<i64>("SELECT 42", ()).await.unwrap(), 42);
    }
}

//! Data access layer — port of `src/db/index.ts`.
//!
//! The TS version wraps `@libsql/client` (SQLite over HTTP to Turso). Here
//! we use `sqlx` with a local SQLite file — the schema in `migrations/` is
//! plain SQLite and is reused verbatim. Repositories (added under
//! `db/repos/`) take `&SqlitePool` so a libsql/Turso client can be swapped
//! in later without touching call sites.

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

use crate::error::Result;

/// Public share-link lifetime — mirrors `PUBLIC_LINK_TTL` in db/index.ts.
pub const PUBLIC_LINK_TTL: i64 = 7 * 24 * 60 * 60;

/// Unix seconds — mirrors `now()` in db/index.ts.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Shared database handle. Cheap to clone — wraps a connection pool.
#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Connect to the database, creating the file if needed.
    /// Does NOT run migrations — call [`Db::migrate`] explicitly so the
    /// migration step stays visible (same convention as `npm run db:setup`
    /// in the TS project, which never migrates at runtime).
    pub async fn connect(url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            .foreign_keys(true);

        // SQLite won't create missing parent directories for us.
        if let Some(parent) = options.get_filename().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        Ok(Self { pool })
    }

    /// Apply pending migrations from `crates/core/migrations/`.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    /// Raw pool access for repositories.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

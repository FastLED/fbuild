//! SQLite-based crash-safe index for the two-phase disk cache.
//!
//! Opened in WAL mode with `synchronous=NORMAL` for multi-reader/single-writer
//! safety and crash recovery. The WAL is replayed on next open after a crash.
//!
//! # Schema migrations
//!
//! Migrations are tracked in the `schema_migrations` table keyed by a
//! stable migration id (`m001_initial_schema`, `m002_add_leases_refcount`,
//! ...). On open, every registered migration is applied in order, inside
//! its own transaction, unless it has already been recorded. This lets
//! older production databases (created before a column existed) pick up
//! schema deltas idempotently — adding a new migration is append-only.
//!
//! The legacy `cache_meta.schema_version` key is still written for
//! backwards compatibility with any tooling that inspects it, but the
//! authoritative source of truth is `schema_migrations`.
//!
//! # Module layout
//!
//! This module is split across several files to keep individual files
//! under the workspace LOC gate:
//!
//! - `mod.rs` — core types ([`CacheEntry`], [`CacheIndex`]) plus
//!   `open`/`migrate`/`schema_version` lifecycle.
//! - `queries.rs` — `impl CacheIndex` block with all lookup / mutation /
//!   lease / LRU / reconciliation methods.
//! - `migrations.rs` — append-only migration list and helpers.
//! - `pid.rs` — platform-specific PID liveness check.
//! - `tests.rs` — the index test suite (gated by `#[cfg(test)]`).
//!
//! The public API is unchanged: external callers continue to refer to
//! `super::index::{CacheIndex, CacheEntry}`.

use super::paths::{self, Kind};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

mod migrations;
mod pid;
mod queries;

#[cfg(test)]
mod tests;

use migrations::MIGRATIONS;

/// Legacy version pin written to `cache_meta` for backwards compatibility.
///
/// The real source of truth is the `schema_migrations` table. This value
/// is mirrored so older tools that read `cache_meta.schema_version` keep
/// working.
const LEGACY_SCHEMA_VERSION: i64 = 1;

/// A row in the `entries` table.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub id: i64,
    pub kind: Kind,
    pub url: String,
    pub stem: String,
    pub hash: String,
    pub version: String,
    pub archive_path: Option<String>,
    pub archive_bytes: Option<i64>,
    pub archive_sha256: Option<String>,
    pub installed_path: Option<String>,
    pub installed_bytes: Option<i64>,
    pub installed_at: Option<i64>,
    pub archived_at: Option<i64>,
    pub last_used_at: i64,
    pub use_count: i64,
    pub pinned: i64,
}

/// The crash-safe SQLite index.
///
/// Wraps the connection in a `Mutex` so the index is `Send + Sync`
/// and can be shared via `Arc` across threads.
pub struct CacheIndex {
    pub(super) conn: Mutex<Connection>,
    cache_root: PathBuf,
}

impl CacheIndex {
    /// Open (or create) the index at the standard location under `cache_root`.
    /// Runs migrations if needed.
    pub fn open(cache_root: &Path) -> rusqlite::Result<Self> {
        let db_path = paths::index_path(cache_root);
        std::fs::create_dir_all(cache_root).map_err(|e| {
            rusqlite::Error::InvalidPath(PathBuf::from(format!(
                "failed to create cache root: {}",
                e
            )))
        })?;
        let conn = Connection::open(&db_path)?;

        // WAL mode for crash safety and concurrent access
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Enable foreign keys
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let idx = Self {
            conn: Mutex::new(conn),
            cache_root: cache_root.to_path_buf(),
        };
        idx.migrate()?;
        Ok(idx)
    }

    /// Open with an in-memory database (for testing).
    #[cfg(test)]
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let idx = Self {
            conn: Mutex::new(conn),
            cache_root: PathBuf::from("/tmp/test_cache"),
        };
        idx.migrate()?;
        Ok(idx)
    }

    fn migrate(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Self::run_migrations(&conn)
    }

    /// Bootstrap the migrations-tracking table and run each registered
    /// migration exactly once. Safe to call against fresh *or* pre-existing
    /// databases; each migration is wrapped in its own transaction.
    fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
        // Bootstrap the migrations-tracking table. We can't use the
        // migration framework itself for this because it's what records
        // whether a migration ran.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                id         TEXT PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );",
        )?;

        for m in MIGRATIONS {
            let already_applied: bool = conn
                .query_row(
                    "SELECT 1 FROM schema_migrations WHERE id = ?1",
                    params![m.id],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if already_applied {
                continue;
            }

            let tx = conn.unchecked_transaction()?;
            (m.up)(&tx)?;
            tx.execute(
                "INSERT INTO schema_migrations (id, applied_at) VALUES (?1, ?2)",
                params![m.id, Self::now_epoch()],
            )?;
            tx.commit()?;
        }

        // Mirror the legacy cache_meta.schema_version key so any tooling
        // that inspects it sees the expected value. Best-effort — the
        // authoritative source is now `schema_migrations`.
        let _ = conn.execute(
            "INSERT OR REPLACE INTO cache_meta (key, value) VALUES ('schema_version', ?1)",
            params![LEGACY_SCHEMA_VERSION.to_string()],
        );

        Ok(())
    }

    /// Get the current schema version.
    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT value FROM cache_meta WHERE key = 'schema_version'",
            [],
            |row| {
                let v: String = row.get(0)?;
                Ok(v.parse::<i64>().unwrap_or(0))
            },
        )
        .optional()
        .map(|opt| opt.unwrap_or(0))
    }

    pub(super) fn now_epoch() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    /// Get a reference to the cache root path.
    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }
}

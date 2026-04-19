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

use super::paths::{self, Kind};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Legacy version pin written to `cache_meta` for backwards compatibility.
///
/// The real source of truth is the `schema_migrations` table. This value
/// is mirrored so older tools that read `cache_meta.schema_version` keep
/// working.
const LEGACY_SCHEMA_VERSION: i64 = 1;

/// A single ordered schema migration. Applied idempotently on open.
///
/// `id` must be stable and unique. Append new migrations to [`MIGRATIONS`]
/// — never reorder or rename existing ids.
struct Migration {
    id: &'static str,
    up: fn(&Connection) -> rusqlite::Result<()>,
}

/// The ordered list of migrations. Append-only.
///
/// - `m001_initial_schema` — the original tables + indexes. Uses
///   `CREATE TABLE IF NOT EXISTS` so it is safe on databases where these
///   objects already exist (e.g. older production caches).
/// - `m002_add_leases_refcount` — adds `leases.refcount` when absent.
///   Older databases (pre-#119) had a `leases` table without this column,
///   which broke `pin()`/`unpin()`. New databases already get it from
///   `m001`, so this migration only mutates old caches.
const MIGRATIONS: &[Migration] = &[
    Migration {
        id: "m001_initial_schema",
        up: migrate_m001_initial_schema,
    },
    Migration {
        id: "m002_add_leases_refcount",
        up: migrate_m002_add_leases_refcount,
    },
];

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
    conn: Mutex<Connection>,
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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

    fn now_epoch() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    /// Look up an entry by kind, url, and version.
    pub fn lookup(
        &self,
        kind: Kind,
        url: &str,
        version: &str,
    ) -> rusqlite::Result<Option<CacheEntry>> {
        let (_, hash) = paths::stem_and_hash(url);
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, kind, url, stem, hash, version,
                    archive_path, archive_bytes, archive_sha256,
                    installed_path, installed_bytes, installed_at,
                    archived_at, last_used_at, use_count, pinned
             FROM entries
             WHERE kind = ?1 AND hash = ?2 AND version = ?3",
            params![kind.as_str(), hash, version],
            Self::row_to_entry,
        )
        .optional()
    }

    /// Record that an archive was downloaded.
    pub fn record_archive(
        &self,
        kind: Kind,
        url: &str,
        version: &str,
        archive_path: &str,
        archive_bytes: i64,
        archive_sha256: &str,
    ) -> rusqlite::Result<CacheEntry> {
        let (stem, hash) = paths::stem_and_hash(url);
        let now = Self::now_epoch();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO entries (kind, url, stem, hash, version,
                                  archive_path, archive_bytes, archive_sha256,
                                  archived_at, last_used_at, use_count, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, 0)
             ON CONFLICT(kind, hash, version) DO UPDATE SET
                 archive_path = excluded.archive_path,
                 archive_bytes = excluded.archive_bytes,
                 archive_sha256 = excluded.archive_sha256,
                 archived_at = excluded.archived_at,
                 last_used_at = excluded.last_used_at",
            params![
                kind.as_str(),
                url,
                stem,
                hash,
                version,
                archive_path,
                archive_bytes,
                archive_sha256,
                now,
                now,
            ],
        )?;
        drop(conn);

        self.lookup(kind, url, version)
            .map(|opt| opt.expect("entry must exist after insert"))
    }

    /// Record that an archive was extracted/installed.
    pub fn record_install(
        &self,
        kind: Kind,
        url: &str,
        version: &str,
        installed_path: &str,
        installed_bytes: i64,
    ) -> rusqlite::Result<CacheEntry> {
        let (stem, hash) = paths::stem_and_hash(url);
        let now = Self::now_epoch();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO entries (kind, url, stem, hash, version,
                                  installed_path, installed_bytes, installed_at,
                                  last_used_at, use_count, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0)
             ON CONFLICT(kind, hash, version) DO UPDATE SET
                 installed_path = excluded.installed_path,
                 installed_bytes = excluded.installed_bytes,
                 installed_at = excluded.installed_at,
                 last_used_at = excluded.last_used_at",
            params![
                kind.as_str(),
                url,
                stem,
                hash,
                version,
                installed_path,
                installed_bytes,
                now,
                now,
            ],
        )?;
        drop(conn);

        self.lookup(kind, url, version)
            .map(|opt| opt.expect("entry must exist after insert"))
    }

    /// Bump LRU timestamp and use count for an entry.
    pub fn touch(&self, entry_id: i64) -> rusqlite::Result<()> {
        let now = Self::now_epoch();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE entries SET last_used_at = ?1, use_count = use_count + 1 WHERE id = ?2",
            params![now, entry_id],
        )?;
        Ok(())
    }

    /// Increment the pinned count for an entry (lease acquired).
    /// Uses a per-(PID, nonce) refcount so multiple `Lease` guards in the same
    /// process correctly track independent pins.
    pub fn pin(&self, entry_id: i64, holder_pid: u32, holder_nonce: u64) -> rusqlite::Result<()> {
        let now = Self::now_epoch();
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE leases SET refcount = refcount + 1
             WHERE entry_id = ?1 AND holder_pid = ?2 AND holder_nonce = ?3",
            params![entry_id, holder_pid as i64, holder_nonce as i64],
        )?;
        if updated == 0 {
            conn.execute(
                "INSERT INTO leases (entry_id, holder_pid, holder_nonce, refcount, acquired_at)
                 VALUES (?1, ?2, ?3, 1, ?4)",
                params![entry_id, holder_pid as i64, holder_nonce as i64, now],
            )?;
        }
        conn.execute(
            "UPDATE entries SET pinned = (SELECT COALESCE(SUM(refcount), 0) FROM leases WHERE entry_id = ?1) WHERE id = ?1",
            params![entry_id],
        )?;
        Ok(())
    }

    /// Decrement the pinned count for an entry (lease released).
    /// Decrements refcount; removes the row when it reaches zero.
    pub fn unpin(&self, entry_id: i64, holder_pid: u32, holder_nonce: u64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE leases SET refcount = refcount - 1
             WHERE entry_id = ?1 AND holder_pid = ?2 AND holder_nonce = ?3",
            params![entry_id, holder_pid as i64, holder_nonce as i64],
        )?;
        conn.execute(
            "DELETE FROM leases
             WHERE entry_id = ?1 AND holder_pid = ?2 AND holder_nonce = ?3 AND refcount <= 0",
            params![entry_id, holder_pid as i64, holder_nonce as i64],
        )?;
        conn.execute(
            "UPDATE entries SET pinned = (SELECT COALESCE(SUM(refcount), 0) FROM leases WHERE entry_id = ?1) WHERE id = ?1",
            params![entry_id],
        )?;
        Ok(())
    }

    /// Get total bytes for all archives.
    pub fn total_archive_bytes(&self) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(SUM(archive_bytes), 0) FROM entries WHERE archive_path IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0),
        )
    }

    /// Get total bytes for all installed entries.
    pub fn total_installed_bytes(&self) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(SUM(installed_bytes), 0) FROM entries WHERE installed_path IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0),
        )
    }

    /// Get LRU installed entries (oldest first), skipping pinned.
    pub fn lru_installed_entries(&self, limit: usize) -> rusqlite::Result<Vec<CacheEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, kind, url, stem, hash, version,
                    archive_path, archive_bytes, archive_sha256,
                    installed_path, installed_bytes, installed_at,
                    archived_at, last_used_at, use_count, pinned
             FROM entries
             WHERE installed_path IS NOT NULL AND pinned = 0
             ORDER BY last_used_at ASC
             LIMIT ?1",
        )?;

        let entries = stmt
            .query_map(params![limit as i64], Self::row_to_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Get LRU archive entries (oldest first), skipping pinned.
    pub fn lru_archive_entries(&self, limit: usize) -> rusqlite::Result<Vec<CacheEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, kind, url, stem, hash, version,
                    archive_path, archive_bytes, archive_sha256,
                    installed_path, installed_bytes, installed_at,
                    archived_at, last_used_at, use_count, pinned
             FROM entries
             WHERE archive_path IS NOT NULL AND pinned = 0
             ORDER BY last_used_at ASC
             LIMIT ?1",
        )?;

        let entries = stmt
            .query_map(params![limit as i64], Self::row_to_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Null out the installed_path for an entry (after evicting its directory).
    pub fn clear_installed(&self, entry_id: i64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE entries SET installed_path = NULL, installed_bytes = NULL, installed_at = NULL WHERE id = ?1",
            params![entry_id],
        )?;
        Ok(())
    }

    /// Null out the archive_path for an entry (after evicting its archive).
    pub fn clear_archive(&self, entry_id: i64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE entries SET archive_path = NULL, archive_bytes = NULL, archive_sha256 = NULL, archived_at = NULL WHERE id = ?1",
            params![entry_id],
        )?;
        Ok(())
    }

    /// Delete an entry entirely (when both archive and installed are gone).
    pub fn delete_entry(&self, entry_id: i64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM entries WHERE id = ?1", params![entry_id])?;
        Ok(())
    }

    /// Reap leases for dead PIDs.
    pub fn reap_dead_leases(&self) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT DISTINCT holder_pid FROM leases")?;
        let pids: Vec<i64> = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        let mut reaped = 0;
        for pid in pids {
            if !is_pid_alive(pid as u32) {
                conn.execute("DELETE FROM leases WHERE holder_pid = ?1", params![pid])?;
                reaped += 1;
            }
        }

        // Refresh pinned counts for all affected entries — use SUM(refcount)
        // to match pin()/unpin() which also use SUM(refcount), not COUNT(*).
        if reaped > 0 {
            conn.execute_batch(
                "UPDATE entries SET pinned = (SELECT COALESCE(SUM(refcount), 0) FROM leases WHERE leases.entry_id = entries.id)",
            )?;
        }

        Ok(reaped)
    }

    /// Get all entries (for reconciliation).
    pub fn all_entries(&self) -> rusqlite::Result<Vec<CacheEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, kind, url, stem, hash, version,
                    archive_path, archive_bytes, archive_sha256,
                    installed_path, installed_bytes, installed_at,
                    archived_at, last_used_at, use_count, pinned
             FROM entries",
        )?;

        let entries = stmt
            .query_map([], Self::row_to_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Look up the most recently used entry for a kind+url pair (any version).
    /// Returns the entry with the highest `last_used_at` that has an installed path.
    pub fn lookup_latest(&self, kind: Kind, url: &str) -> rusqlite::Result<Option<CacheEntry>> {
        let (_, hash) = paths::stem_and_hash(url);
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, kind, url, stem, hash, version,
                    archive_path, archive_bytes, archive_sha256,
                    installed_path, installed_bytes, installed_at,
                    archived_at, last_used_at, use_count, pinned
             FROM entries
             WHERE kind = ?1 AND hash = ?2 AND installed_path IS NOT NULL
             ORDER BY last_used_at DESC
             LIMIT 1",
            params![kind.as_str(), hash],
            Self::row_to_entry,
        )
        .optional()
    }

    /// Get total entry count.
    pub fn entry_count(&self) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM entries", [], |row| {
            row.get::<_, i64>(0)
        })
    }

    /// Get a reference to the cache root path.
    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<CacheEntry> {
        let kind_str: String = row.get(1)?;
        let kind: Kind = kind_str.parse().map_err(|e: String| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })?;
        Ok(CacheEntry {
            id: row.get(0)?,
            kind,
            url: row.get(2)?,
            stem: row.get(3)?,
            hash: row.get(4)?,
            version: row.get(5)?,
            archive_path: row.get(6)?,
            archive_bytes: row.get(7)?,
            archive_sha256: row.get(8)?,
            installed_path: row.get(9)?,
            installed_bytes: row.get(10)?,
            installed_at: row.get(11)?,
            archived_at: row.get(12)?,
            last_used_at: row.get(13)?,
            use_count: row.get(14)?,
            pinned: row.get(15)?,
        })
    }
}

/// m001 — original schema. Creates `cache_meta`, `entries`, the LRU
/// indexes, and `leases` (with `refcount`). Uses `IF NOT EXISTS` so it
/// is safe against caches that already have these objects from the
/// pre-migration-framework era.
fn migrate_m001_initial_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cache_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS entries (
            id              INTEGER PRIMARY KEY,
            kind            TEXT NOT NULL,
            url             TEXT NOT NULL,
            stem            TEXT NOT NULL,
            hash            TEXT NOT NULL,
            version         TEXT NOT NULL,
            archive_path    TEXT,
            archive_bytes   INTEGER,
            archive_sha256  TEXT,
            installed_path  TEXT,
            installed_bytes INTEGER,
            installed_at    INTEGER,
            archived_at     INTEGER,
            last_used_at    INTEGER NOT NULL,
            use_count       INTEGER NOT NULL DEFAULT 0,
            pinned          INTEGER NOT NULL DEFAULT 0,
            UNIQUE(kind, hash, version)
        );

        CREATE INDEX IF NOT EXISTS idx_lru_installed
            ON entries(last_used_at) WHERE installed_path IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_lru_archive
            ON entries(last_used_at) WHERE archive_path IS NOT NULL;

        CREATE TABLE IF NOT EXISTS leases (
            entry_id     INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
            holder_pid   INTEGER NOT NULL,
            holder_nonce INTEGER NOT NULL DEFAULT 0,
            refcount     INTEGER NOT NULL DEFAULT 1,
            acquired_at  INTEGER NOT NULL,
            PRIMARY KEY(entry_id, holder_pid, holder_nonce)
        );",
    )?;
    Ok(())
}

/// m002 — add `leases.refcount` on pre-existing caches.
///
/// Older fbuild versions created `leases` without the `refcount` column.
/// New caches already get it from `m001_initial_schema`, so this
/// migration is a no-op for them. For old ones, we `ALTER TABLE` to add
/// the column with `DEFAULT 1` (matching the semantic that existing rows
/// represent a single held lease) and then rebaseline to `1` in case the
/// column was added but left at its implicit NULL by older sqlite.
fn migrate_m002_add_leases_refcount(conn: &Connection) -> rusqlite::Result<()> {
    if leases_has_refcount(conn)? {
        return Ok(());
    }
    conn.execute_batch(
        "ALTER TABLE leases ADD COLUMN refcount INTEGER NOT NULL DEFAULT 1;
         UPDATE leases SET refcount = 1 WHERE refcount IS NULL OR refcount <= 0;",
    )?;
    Ok(())
}

/// Returns true iff the `leases` table has a column named `refcount`.
/// Used by migrations to decide whether an `ALTER TABLE` is needed.
fn leases_has_refcount(conn: &Connection) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(leases)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for col in rows {
        if col? == "refcount" {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Check if a PID is alive. Platform-specific.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks if process exists without sending a signal.
        // Use raw FFI to avoid a libc crate dependency (matters for musl builds).
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        unsafe { kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        // Use OpenProcess to check if PID is alive (fast, no subprocess).
        // PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
        extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            false
        } else {
            unsafe { CloseHandle(handle) };
            true
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_open_creates_schema() {
        let tmp = tempfile::TempDir::new().unwrap();
        let idx = CacheIndex::open(tmp.path()).unwrap();
        assert_eq!(idx.schema_version().unwrap(), 1);
        assert_eq!(idx.entry_count().unwrap(), 0);
    }

    #[test]
    fn test_index_reopen_preserves_data() {
        let tmp = tempfile::TempDir::new().unwrap();
        {
            let idx = CacheIndex::open(tmp.path()).unwrap();
            idx.record_archive(
                Kind::Packages,
                "https://example.com/pkg.tar.gz",
                "1.0.0",
                "archives/packages/example-pkg/abc123/1.0.0/pkg.tar.gz",
                1024,
                "sha256abc",
            )
            .unwrap();
        }
        // Reopen
        let idx = CacheIndex::open(tmp.path()).unwrap();
        let entry = idx
            .lookup(Kind::Packages, "https://example.com/pkg.tar.gz", "1.0.0")
            .unwrap();
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.archive_bytes, Some(1024));
    }

    #[test]
    fn test_record_archive_then_install_roundtrip() {
        let idx = CacheIndex::open_in_memory().unwrap();
        let url = "https://example.com/tool.tar.gz";

        let entry = idx
            .record_archive(
                Kind::Toolchains,
                url,
                "7.3.0",
                "archives/tool.tar.gz",
                5000,
                "deadbeef",
            )
            .unwrap();
        assert_eq!(entry.kind, Kind::Toolchains);
        assert_eq!(entry.version, "7.3.0");
        assert_eq!(entry.archive_bytes, Some(5000));
        assert!(entry.installed_path.is_none());

        let entry = idx
            .record_install(
                Kind::Toolchains,
                url,
                "7.3.0",
                "installed/toolchains/tool/abc/7.3.0",
                20000,
            )
            .unwrap();
        assert_eq!(entry.archive_bytes, Some(5000)); // archive still there
        assert_eq!(entry.installed_bytes, Some(20000));
        assert!(entry.installed_path.is_some());
    }

    #[test]
    fn test_lookup_returns_none_for_missing() {
        let idx = CacheIndex::open_in_memory().unwrap();
        let result = idx
            .lookup(Kind::Packages, "https://example.com/nope", "1.0.0")
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_touch_bumps_use_count() {
        let idx = CacheIndex::open_in_memory().unwrap();
        let entry = idx
            .record_archive(
                Kind::Packages,
                "https://example.com/a",
                "1.0",
                "path",
                100,
                "sha",
            )
            .unwrap();
        assert_eq!(entry.use_count, 0);

        idx.touch(entry.id).unwrap();
        let entry = idx
            .lookup(Kind::Packages, "https://example.com/a", "1.0")
            .unwrap()
            .unwrap();
        assert_eq!(entry.use_count, 1);

        idx.touch(entry.id).unwrap();
        let entry = idx
            .lookup(Kind::Packages, "https://example.com/a", "1.0")
            .unwrap()
            .unwrap();
        assert_eq!(entry.use_count, 2);
    }

    #[test]
    fn test_pin_and_unpin() {
        let idx = CacheIndex::open_in_memory().unwrap();
        let entry = idx
            .record_archive(
                Kind::Packages,
                "https://example.com/a",
                "1.0",
                "path",
                100,
                "sha",
            )
            .unwrap();
        assert_eq!(entry.pinned, 0);

        idx.pin(entry.id, 12345, 1).unwrap();
        let entry = idx
            .lookup(Kind::Packages, "https://example.com/a", "1.0")
            .unwrap()
            .unwrap();
        assert_eq!(entry.pinned, 1);

        // Pin with another PID
        idx.pin(entry.id, 67890, 2).unwrap();
        let entry = idx
            .lookup(Kind::Packages, "https://example.com/a", "1.0")
            .unwrap()
            .unwrap();
        assert_eq!(entry.pinned, 2);

        // Unpin one
        idx.unpin(entry.id, 12345, 1).unwrap();
        let entry = idx
            .lookup(Kind::Packages, "https://example.com/a", "1.0")
            .unwrap()
            .unwrap();
        assert_eq!(entry.pinned, 1);
    }

    #[test]
    fn test_lru_installed_entries_skip_pinned() {
        let idx = CacheIndex::open_in_memory().unwrap();

        // Create two installed entries
        let e1 = idx
            .record_install(
                Kind::Packages,
                "https://example.com/a",
                "1.0",
                "path_a",
                1000,
            )
            .unwrap();
        let _e2 = idx
            .record_install(
                Kind::Packages,
                "https://example.com/b",
                "1.0",
                "path_b",
                2000,
            )
            .unwrap();

        // Pin e1
        idx.pin(e1.id, 99999, 1).unwrap();

        // LRU should only return e2 (unpinned)
        let lru = idx.lru_installed_entries(10).unwrap();
        assert_eq!(lru.len(), 1);
        assert_eq!(lru[0].url, "https://example.com/b");
    }

    #[test]
    fn test_clear_installed_nulls_fields() {
        let idx = CacheIndex::open_in_memory().unwrap();
        let url = "https://example.com/pkg";
        idx.record_archive(Kind::Packages, url, "1.0", "archive_path", 100, "sha")
            .unwrap();
        let entry = idx
            .record_install(Kind::Packages, url, "1.0", "install_path", 500)
            .unwrap();

        idx.clear_installed(entry.id).unwrap();
        let entry = idx.lookup(Kind::Packages, url, "1.0").unwrap().unwrap();
        assert!(entry.installed_path.is_none());
        assert!(entry.installed_bytes.is_none());
        // Archive still present
        assert!(entry.archive_path.is_some());
    }

    #[test]
    fn test_clear_archive_nulls_fields() {
        let idx = CacheIndex::open_in_memory().unwrap();
        let url = "https://example.com/pkg";
        let entry = idx
            .record_archive(Kind::Packages, url, "1.0", "archive_path", 100, "sha")
            .unwrap();

        idx.clear_archive(entry.id).unwrap();
        let entry = idx.lookup(Kind::Packages, url, "1.0").unwrap().unwrap();
        assert!(entry.archive_path.is_none());
        assert!(entry.archive_bytes.is_none());
        assert!(entry.archive_sha256.is_none());
    }

    #[test]
    fn test_delete_entry() {
        let idx = CacheIndex::open_in_memory().unwrap();
        let entry = idx
            .record_archive(
                Kind::Packages,
                "https://example.com/a",
                "1.0",
                "path",
                100,
                "sha",
            )
            .unwrap();
        assert_eq!(idx.entry_count().unwrap(), 1);

        idx.delete_entry(entry.id).unwrap();
        assert_eq!(idx.entry_count().unwrap(), 0);
    }

    #[test]
    fn test_total_bytes_accounting() {
        let idx = CacheIndex::open_in_memory().unwrap();
        assert_eq!(idx.total_archive_bytes().unwrap(), 0);
        assert_eq!(idx.total_installed_bytes().unwrap(), 0);

        idx.record_archive(Kind::Packages, "https://a.com/a", "1.0", "p1", 1000, "s1")
            .unwrap();
        idx.record_archive(Kind::Packages, "https://b.com/b", "1.0", "p2", 2000, "s2")
            .unwrap();
        assert_eq!(idx.total_archive_bytes().unwrap(), 3000);

        idx.record_install(Kind::Toolchains, "https://c.com/c", "1.0", "p3", 5000)
            .unwrap();
        assert_eq!(idx.total_installed_bytes().unwrap(), 5000);
    }

    #[test]
    fn test_reconcile_orphan_row_nulled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let idx = CacheIndex::open(tmp.path()).unwrap();

        let entry = idx
            .record_archive(
                Kind::Packages,
                "https://example.com/orphan",
                "1.0",
                "archives/packages/orphan/hash/1.0/pkg.tar.gz",
                500,
                "sha",
            )
            .unwrap();

        assert!(entry.archive_path.is_some());
        idx.clear_archive(entry.id).unwrap();
        let entry = idx
            .lookup(Kind::Packages, "https://example.com/orphan", "1.0")
            .unwrap()
            .unwrap();
        assert!(entry.archive_path.is_none());
        assert_eq!(idx.entry_count().unwrap(), 1);
    }

    /// A freshly-migrated database must report every registered migration
    /// as applied — regression guard against adding a migration to
    /// `MIGRATIONS` but forgetting to record it.
    #[test]
    fn test_all_migrations_recorded_after_open() {
        let tmp = tempfile::TempDir::new().unwrap();
        let idx = CacheIndex::open(tmp.path()).unwrap();
        let conn = idx.conn.lock().unwrap();
        for m in MIGRATIONS {
            let applied: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM schema_migrations WHERE id = ?1",
                    params![m.id],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();
            assert!(applied.is_some(), "migration {} not recorded", m.id);
        }
    }

    /// Pre-migration schema: `leases` without `refcount`. Opening via
    /// `CacheIndex::open` must transparently add the column, and
    /// subsequent `pin()` calls must succeed.
    #[test]
    fn test_legacy_schema_missing_refcount_is_migrated() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = paths::index_path(tmp.path());
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

        // Hand-craft the legacy schema: leases has no refcount column.
        {
            let raw = Connection::open(&db_path).unwrap();
            raw.execute_batch(
                "CREATE TABLE cache_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE entries (
                    id              INTEGER PRIMARY KEY,
                    kind            TEXT NOT NULL,
                    url             TEXT NOT NULL,
                    stem            TEXT NOT NULL,
                    hash            TEXT NOT NULL,
                    version         TEXT NOT NULL,
                    archive_path    TEXT,
                    archive_bytes   INTEGER,
                    archive_sha256  TEXT,
                    installed_path  TEXT,
                    installed_bytes INTEGER,
                    installed_at    INTEGER,
                    archived_at     INTEGER,
                    last_used_at    INTEGER NOT NULL,
                    use_count       INTEGER NOT NULL DEFAULT 0,
                    pinned          INTEGER NOT NULL DEFAULT 0,
                    UNIQUE(kind, hash, version)
                 );
                 CREATE TABLE leases (
                    entry_id     INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
                    holder_pid   INTEGER NOT NULL,
                    holder_nonce INTEGER NOT NULL DEFAULT 0,
                    acquired_at  INTEGER NOT NULL,
                    PRIMARY KEY(entry_id, holder_pid, holder_nonce)
                 );
                 INSERT INTO cache_meta(key, value) VALUES ('schema_version', '1');",
            )
            .unwrap();
            // raw goes out of scope → connection closes and file is flushed.
        }

        // Re-open through the normal path. Migrations should run.
        let idx = CacheIndex::open(tmp.path()).unwrap();
        assert!(
            leases_has_refcount(&idx.conn.lock().unwrap()).unwrap(),
            "m002 should have added leases.refcount"
        );

        // pin()/unpin() must now succeed end-to-end.
        let entry = idx
            .record_archive(Kind::Packages, "https://example.com/x", "1.0", "p", 1, "s")
            .unwrap();
        idx.pin(entry.id, 4242, 7).unwrap();
        let entry = idx
            .lookup(Kind::Packages, "https://example.com/x", "1.0")
            .unwrap()
            .unwrap();
        assert_eq!(entry.pinned, 1);
        idx.unpin(entry.id, 4242, 7).unwrap();
    }

    /// Migrations must be idempotent: opening twice must not re-apply
    /// them and must not double-error on the `ALTER TABLE`.
    #[test]
    fn test_migrations_idempotent_across_reopens() {
        let tmp = tempfile::TempDir::new().unwrap();
        {
            let _idx = CacheIndex::open(tmp.path()).unwrap();
        }
        // Second open must succeed — no "duplicate column name" error.
        let idx = CacheIndex::open(tmp.path()).unwrap();
        assert!(leases_has_refcount(&idx.conn.lock().unwrap()).unwrap());
    }
}

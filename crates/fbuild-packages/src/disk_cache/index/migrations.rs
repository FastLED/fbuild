//! Append-only schema migrations for the SQLite cache index.
//!
//! Each migration has a stable id (e.g. `m001_initial_schema`) recorded in
//! the `schema_migrations` table. Append new migrations to [`MIGRATIONS`] —
//! never reorder or rename existing ids.

use rusqlite::Connection;

/// A single ordered schema migration. Applied idempotently on open.
///
/// `id` must be stable and unique. Append new migrations to [`MIGRATIONS`]
/// — never reorder or rename existing ids.
pub(super) struct Migration {
    pub(super) id: &'static str,
    pub(super) up: fn(&Connection) -> rusqlite::Result<()>,
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
pub(super) const MIGRATIONS: &[Migration] = &[
    Migration {
        id: "m001_initial_schema",
        up: migrate_m001_initial_schema,
    },
    Migration {
        id: "m002_add_leases_refcount",
        up: migrate_m002_add_leases_refcount,
    },
];

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
pub(super) fn leases_has_refcount(conn: &Connection) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(leases)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for col in rows {
        if col? == "refcount" {
            return Ok(true);
        }
    }
    Ok(false)
}

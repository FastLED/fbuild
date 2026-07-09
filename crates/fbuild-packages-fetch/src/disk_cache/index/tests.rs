//! Tests for the SQLite cache index.

use rusqlite::{params, Connection, OptionalExtension};

use super::super::paths::{self, Kind};
use super::migrations::{leases_has_refcount, MIGRATIONS};
use super::CacheIndex;

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

/// Existing migration ids are compatibility state. New migrations may be
/// appended, but historical ids must not be renamed or reordered because
/// production databases record them in `schema_migrations`.
#[test]
fn test_existing_migration_ids_remain_append_only() {
    let expected_prefix = ["m001_initial_schema", "m002_add_leases_refcount"];
    let ids: Vec<&str> = MIGRATIONS.iter().map(|m| m.id).collect();

    assert!(
        ids.starts_with(&expected_prefix),
        "existing migration ids must remain a stable prefix"
    );

    let unique_ids: std::collections::HashSet<&str> = ids.iter().copied().collect();
    assert_eq!(
        unique_ids.len(),
        ids.len(),
        "migration ids must remain unique"
    );
}

/// Dead-process leases must not pin cache entries forever. Reaping should
/// remove stale lease rows and refresh the denormalized `entries.pinned`
/// count from the remaining live leases.
#[test]
fn test_dead_pid_lease_reap_clears_pinned_count() {
    let idx = CacheIndex::open_in_memory().unwrap();
    let entry = idx
        .record_archive(
            Kind::Packages,
            "https://example.com/dead-lease",
            "1.0",
            "archives/dead-lease.tar.gz",
            1024,
            "sha256",
        )
        .unwrap();

    let dead_pid = (900_000_u32..1_000_000)
        .find(|pid| !super::pid::is_pid_alive(*pid))
        .expect("test requires an unused high PID") as i64;
    {
        let conn = idx.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO leases (entry_id, holder_pid, holder_nonce, refcount, acquired_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![entry.id, dead_pid, 7_i64, 3_i64, CacheIndex::now_epoch()],
        )
        .unwrap();
        conn.execute(
            "UPDATE entries SET pinned = 3 WHERE id = ?1",
            params![entry.id],
        )
        .unwrap();
    }

    let before = idx
        .lookup(Kind::Packages, "https://example.com/dead-lease", "1.0")
        .unwrap()
        .unwrap();
    assert_eq!(before.pinned, 3);

    assert_eq!(idx.reap_dead_leases().unwrap(), 1);

    let after = idx
        .lookup(Kind::Packages, "https://example.com/dead-lease", "1.0")
        .unwrap()
        .unwrap();
    assert_eq!(after.pinned, 0);

    let conn = idx.conn.lock().unwrap();
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM leases", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 0);
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

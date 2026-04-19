//! Integration test for `DiskCache` schema migration on legacy databases.
//!
//! The original `leases` table (created before PR #119) had no `refcount`
//! column. Opening such a database via `DiskCache::open_at` must
//! transparently migrate it, and `DiskCache::lease()` must then succeed
//! instead of erroring with "no such column: refcount".
//!
//! See: https://github.com/FastLED/fbuild/issues/124

use fbuild_packages::disk_cache::Kind;
use fbuild_packages::DiskCache;
use rusqlite::Connection;
use std::path::PathBuf;

/// Reproduce the exact filesystem layout `DiskCache::open_at` expects for
/// a *pre-migration* cache. Mirrors `disk_cache::paths::index_path`.
fn index_path(cache_root: &std::path::Path) -> PathBuf {
    cache_root.join("index.sqlite")
}

/// Hand-craft the legacy schema that production caches from before the
/// `leases.refcount` column existed. No `refcount`, no `schema_migrations`
/// table, but a legacy `cache_meta.schema_version = '1'` marker.
fn seed_legacy_schema(db_path: &std::path::Path) {
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let raw = Connection::open(db_path).unwrap();
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
    // `raw` drops here, closing the connection and flushing to disk.
}

/// Opening an older DB (no `leases.refcount`) must migrate it on the fly,
/// and `DiskCache::lease()` must succeed afterwards. This is the exact
/// production scenario reported in issue #124.
#[test]
fn legacy_cache_migrates_and_lease_succeeds() {
    let tmp = tempfile::TempDir::new().unwrap();
    seed_legacy_schema(&index_path(tmp.path()));

    let cache = DiskCache::open_at(tmp.path()).expect("migration-aware open must succeed");

    // Record an entry so there's something to lease.
    let entry = cache
        .record_install(
            Kind::LnkBlobs,
            "https://example.com/legacy.bin",
            "deadbeef",
            "installed/legacy/deadbeef",
            128,
        )
        .unwrap();

    // The lease call is what used to fail with:
    //   "no such column: refcount in UPDATE leases SET refcount = ..."
    let lease = cache
        .lease(&entry)
        .expect("lease must succeed after schema migration");

    // Pinned count should reflect the held lease.
    let entry = cache
        .lookup(Kind::LnkBlobs, "https://example.com/legacy.bin", "deadbeef")
        .unwrap()
        .unwrap();
    assert_eq!(entry.pinned, 1);

    // Releasing drops the pin.
    drop(lease);
    let entry = cache
        .lookup(Kind::LnkBlobs, "https://example.com/legacy.bin", "deadbeef")
        .unwrap()
        .unwrap();
    assert_eq!(entry.pinned, 0);
}

/// Regression guard: a fresh cache (no seeded legacy schema) must behave
/// identically. Covers the common happy path so a future migration change
/// can't silently break new users.
#[test]
fn fresh_cache_lease_still_works() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cache = DiskCache::open_at(tmp.path()).unwrap();

    let entry = cache
        .record_install(
            Kind::Packages,
            "https://example.com/fresh",
            "1.0",
            "installed/fresh/1.0",
            256,
        )
        .unwrap();

    let _lease = cache.lease(&entry).expect("fresh cache lease must succeed");
    let entry = cache
        .lookup(Kind::Packages, "https://example.com/fresh", "1.0")
        .unwrap()
        .unwrap();
    assert_eq!(entry.pinned, 1);
}

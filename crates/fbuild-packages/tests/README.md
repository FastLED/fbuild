# fbuild-packages integration tests

Integration tests for the `fbuild-packages` crate. Each `*.rs` file here is
compiled as its own binary by `cargo test` (separate from the unit tests
inside `src/`).

## Tests

- **`lnk_e2e.rs`** — end-to-end test of the `.lnk` resource pipeline
  (scan → resolve → materialize) against a local axum server.
- **`disk_cache_schema_migration.rs`** — verifies `DiskCache::open_at`
  migrates older databases that lack the `leases.refcount` column added
  in PR #119. Regression guard for issue #124.

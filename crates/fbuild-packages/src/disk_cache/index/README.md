# disk_cache/index

SQLite-backed crash-safe index for the two-phase disk cache.

Split across multiple files so each stays under the workspace LOC gate:

- `mod.rs` — core types (`CacheEntry`, `CacheIndex`), open / migrate / lifecycle.
- `queries.rs` — lookup, mutation, lease bookkeeping, LRU, reconciliation queries.
- `migrations.rs` — append-only ordered migrations and helpers.
- `pid.rs` — platform-specific PID liveness check used for lease reaping.
- `tests.rs` — index test suite (gated by `#[cfg(test)]`).

External callers still import as `super::index::{CacheIndex, CacheEntry}`.

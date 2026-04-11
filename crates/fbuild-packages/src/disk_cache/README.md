# disk_cache

Two-phase disk cache with LRU garbage collection, sized budgets, and crash-safe SQLite index.

Separates downloaded archives from installed (extracted) content, allowing cheap-to-rehydrate
installed directories to be evicted before expensive-to-fetch archive blobs.

## Modules

- `mod.rs` - Public `DiskCache` facade
- `paths.rs` - Sole source of cache path construction (archives, installed, staging)
- `index.rs` - SQLite open/migrate/query/touch/reconcile
- `budget.rs` - Size accounting, watermark math, auto-scaling from disk space
- `gc.rs` - Eviction loop, lease reaping, lock handling
- `lease.rs` - RAII `Lease` guard that pins entries during builds

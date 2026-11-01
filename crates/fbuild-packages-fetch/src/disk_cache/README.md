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
- `reclaim.rs` - On-demand, budgeted eviction in a fixed order (FastLED/fbuild#1750 item 3)
- `lease.rs` - RAII `Lease` guard that pins entries during builds

## On-demand reclaim vs. background GC

`gc.rs` enforces watermarks whenever the daemon decides to; `reclaim.rs` runs
only when a user asks (`fbuild cache reclaim`) and stops as soon as the
requested byte count is free. The two share the pin/lease safety model (a
pinned entry is invisible to both) but not the policy:

| | `gc` | `reclaim` |
| --- | --- | --- |
| Trigger | background loop / `fbuild purge` | explicit CLI command |
| Target | low watermark, #20 budgets | a byte count (`--needed`, else the pre-flight shortfall) |
| Order | installed, then archives, across all kinds | `RECLAIM_ORDER`: toolchains → packages → platforms → libraries → frameworks → lnk-blobs, installed before archives within each |
| Mid-build | allowed (that is #1341's bug) | refused while `daemon_status.json` reports an operation in progress |

The zccache object cache is not evicted by `reclaim`: its LRU budget lives
inside the embedded zccache service in `fbuild-daemon`, and there is no
cross-process trim API. A short run says so instead of pretending.

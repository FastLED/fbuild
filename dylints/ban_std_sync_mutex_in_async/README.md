# `ban_std_sync_mutex_in_async`

Custom [dylint](https://github.com/trailofbits/dylint) that forbids
`std::sync::Mutex<T>` and `std::sync::RwLock<T>` *type uses* inside
files under `crates/fbuild-daemon/src/` and
`crates/fbuild-serial/src/`.

## Why

Holding a `std::sync::Mutex` guard across an `.await` point is two
bugs at once:

1. **Scheduler starvation.** While the guard is held, the executor
   cannot park the task — the mutex doesn't know about Tokio's
   futures. Other tasks on the same worker thread stall.
2. **Poison panic.** A panic inside an async section that's holding
   the guard poisons the mutex; every subsequent `.lock()` call
   panics. In a long-running daemon, one panic per type makes the
   whole subsystem unusable.

The fix is to use `tokio::sync::Mutex` / `tokio::sync::RwLock` (or
`parking_lot::Mutex` for hot non-async paths that never cross an
await), or to restructure so the std mutex is released *before* any
await.

## Scope

Only files whose path matches one of these prefixes are linted:

```
crates/fbuild-daemon/src/**/*.rs
crates/fbuild-serial/src/**/*.rs
```

Test files inside scope (`crates/fbuild-daemon/src/**/*tests*.rs`,
`crates/fbuild-serial/src/**/*tests*.rs`) are exempt — synchronous
test plumbing legitimately uses `std::sync::Mutex` to share state
between background threads.

Everything else (other crates, integration tests, examples) is out of
scope.

## Allowlist

Files in scope that legitimately use a `std::sync::Mutex` /
`std::sync::RwLock` (typically because the lock is genuinely held by
a synchronous code path — e.g. `device_manager.rs`,
`status_manager.rs`, the per-session output buffer in
`fbuild-serial/src/manager.rs`) are listed in `src/allowlist.txt`.

The goal is to lock in current state and prevent NEW violations from
spreading; the existing entries are not aspirational targets for
migration. Each entry has an inline comment explaining the
synchronous discipline that justifies it.

## Toolchain

Pinned to the same `nightly-2026-03-26` channel and the same
`trailofbits/dylint` git rev (`4bd91ce…`) the other fbuild dylints
use.

## Running locally

See `dylints/README.md` for the full local-run recipe. CI runs all
dylints on every push/PR via `.github/workflows/dylint.yml`.

## See also

- Issue #826 — this lint's tracking issue

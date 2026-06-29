# `ban_file_based_locks`

This lint bans file-based locking primitives in fbuild production code
(`crates/*/src/`):

- `fs2::FileExt` and `fs2::lock_*` (the `fs2` crate's POSIX-flavored API)
- `OpenOptions::create_new(true)` (the rename-or-fail lock-file pattern)
- raw `flock(...)` system-call wrappers

## Why

Per `CLAUDE.md`'s "Key Constraints" section, fbuild has **no file-based
locks** — all locking flows through the daemon's in-memory managers.
File-based locks regress that contract: they leak across crashes on
Windows, race with watch-set invalidation, and can deadlock in CI when
two workers share a stale lock-file path.

The lint is ON and the allowlist is empty: every current call site is
already routed through the daemon. The lint locks the invariant in so
future code can't drift back.

## Scope

Only files whose path contains BOTH `crates/` and a subsequent `/src/`
segment are linted. `tests/`, `benches/`, `examples/`, build scripts,
and the `ci/` tree are out of scope by design — integration tests can
legitimately probe lock-file collision behavior.

## See also

- FastLED/fbuild#826 — the dylint sweep tracking issue
- `CLAUDE.md` "Key Constraints" — "No file-based locks"

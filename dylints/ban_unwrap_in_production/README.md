# `ban_unwrap_in_production`

Custom [dylint](https://github.com/trailofbits/dylint) that forbids
`.unwrap()` method calls inside fbuild *production* code:

- `crates/fbuild-daemon/src/**/*.rs`
- `crates/fbuild-cli/src/cli/**/*.rs`

Test code (`#[cfg(test)] mod tests { ... }`, sibling `tests.rs`,
`*_tests.rs`, `tests_*.rs` files) is exempt — the lint fires only on
production code.

## Why

In the daemon, panics in any code path (HTTP/WebSocket handler, broker
worker, lock manager, build dispatcher) crash the process, which
disconnects every connected client (CLI build streams, monitor
WebSockets, FastLED's `SerialMonitor`, the FastAPI bridge). Production
code must convert errors into structured HTTP responses, WS error
frames, or `Result` returns instead of panicking.

In the CLI, panics drop the user into a Rust backtrace instead of a
clean error message and non-zero exit, which is poor UX and obscures
the real problem.

PR #833 fixed the original daemon-handler scope (10 known violations).
FastLED/fbuild#844 item 11 widened the scope to all of
`fbuild-daemon/src/` and `fbuild-cli/src/cli/`, sweeping the remaining
production unwrap sites in a single pass.

## Scope

The lint runs only on files whose POSIX-normalized path contains one
of:

```
crates/fbuild-daemon/src/
crates/fbuild-cli/src/cli/
```

Within scope, the following are exempt:

- Files whose name matches `tests.rs`, `*_tests.rs`, or `tests_*.rs`
  (sibling test files).
- `.unwrap()` calls inside a module under a `#[cfg(test)]` attribute
  (the lint walks up the owning module chain).

Everything else (the rest of `fbuild-cli`, all other crates) is out of
scope.

## Allowlist

This lint has no per-call allowlist file. Either the call site is in
test code (exempt automatically) or it is a real production bug that
must be fixed. If you genuinely need an unwrap (e.g. serializing a
known-good struct that cannot fail), prefer
`.unwrap_or_else(|e| e.into_inner())` for poison-tolerant `Mutex::lock`,
`.unwrap_or_default()`, `.expect("…")` with a post-mortem-friendly
invariant message, or propagate with `?` — and document the invariant
inline.

## Toolchain

Pinned to the same `nightly-2026-03-26` channel and the same
`trailofbits/dylint` git rev (`4bd91ce…`) the other fbuild dylints
use.

## Running locally

See `dylints/README.md` for the full local-run recipe. CI runs all
dylints on every push/PR via `.github/workflows/dylint.yml`.

## See also

- Issue #826 — original tracking issue
- PR #833 — initial hardening of daemon handlers
- FastLED/fbuild#844 item 11 — production-scope widening + sibling
  test-file detection refinement

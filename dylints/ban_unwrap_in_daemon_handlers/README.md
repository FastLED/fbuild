# `ban_unwrap_in_daemon_handlers`

Custom [dylint](https://github.com/trailofbits/dylint) that forbids
`.unwrap()` method calls inside files under
`crates/fbuild-daemon/src/handlers/`. Test code (`#[cfg(test)] mod
tests { ... }`, `tests.rs`, `*_tests.rs` files) is exempt — the lint
fires only on production handler code.

## Why

Panics inside HTTP/WebSocket handler paths crash the daemon, which
disconnects every connected client (CLI build streams, monitor
WebSockets, FastLED's `SerialMonitor`, the FastAPI bridge). Production
handlers must convert errors into structured HTTP responses or WS
error frames instead of unwrapping.

PR #833 already fixed 10 known `.unwrap()` violations in daemon
handlers (see the websocket hardening section of that PR). This lint
locks in that state and prevents new violations from sneaking in.

## Scope

The lint runs only on files matching:

```
crates/fbuild-daemon/src/handlers/**/*.rs
```

Within scope, the following are exempt:

- Files whose name matches `*tests*.rs` (e.g. `websockets_tests.rs`,
  `tests_npm_cache.rs`, `tests_process.rs`,
  `tests_select_runner.rs`).
- `.unwrap()` calls inside a module under a `#[cfg(test)]` attribute
  (the lint walks up the owning module chain).

Everything else (`crates/fbuild-daemon/src/{context,broker,...}.rs`,
the rest of the daemon, all other crates) is out of scope.

## Allowlist

This lint has no per-call allowlist file. Either the call site is in
test code (exempt automatically) or it is a real handler bug that must
be fixed. If you genuinely need an unwrap in a handler (e.g.
serializing a known-good struct that cannot fail), prefer
`.unwrap_or_default()`, `.expect("…")`, or a structured error response
— and document the invariant inline.

## Toolchain

Pinned to the same `nightly-2026-03-26` channel and the same
`trailofbits/dylint` git rev (`4bd91ce…`) the other fbuild dylints
use.

## Running locally

See `dylints/README.md` for the full local-run recipe. CI runs all
dylints on every push/PR via `.github/workflows/dylint.yml`.

## See also

- Issue #826 — this lint's tracking issue
- PR #833 — original hardening that fixed the existing 10 unwrap
  violations

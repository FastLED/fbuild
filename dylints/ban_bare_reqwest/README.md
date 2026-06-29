# `ban_bare_reqwest`

Custom [dylint](https://github.com/trailofbits/dylint) that forbids direct
construction of `reqwest::Client` / `reqwest::ClientBuilder` and direct calls
to `reqwest::get` / `reqwest::blocking::get` in fbuild production code
(anything under `crates/*/src/`), except inside the bridge modules
themselves.

## Why

All HTTP traffic in fbuild flows through `fbuild_core::http::client()` so
the workspace shares one configured timeout matrix, one TLS configuration,
and one reqwest dependency surface. Bypassing the bridge silently regresses
those invariants.

See FastLED/fbuild#844 (bridge sweep, "Bridge pair 1") for context.

## Replacement

```rust,ignore
// Shared async client (300s total, 30s connect):
let client = fbuild_core::http::client();

// Per-call timeout override:
let client = fbuild_core::http::client_with_timeout(Duration::from_secs(10));

// Rare OS-thread case (e.g. port_scan):
let client = fbuild_core::http::blocking_client(Duration::from_secs(10));
```

## Scope

Production code only: files whose path contains both `crates/` and a
subsequent `/src/` segment. Tests, examples, and benches are out of scope
by design.

## Allowlist

Empty by design — Phase 2 of #844 migrates every direct construction site.
The bridge modules (`crates/fbuild-core/src/http.rs` and the forward-compat
re-export at `crates/fbuild-packages/src/http.rs`) are exempted by file
path in `lib.rs`.

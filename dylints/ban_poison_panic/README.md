# `ban_poison_panic`

Custom [dylint](https://github.com/trailofbits/dylint) for the fbuild
internal bridge sweep (FastLED/fbuild#844).

## What

.unwrap()/.expect() on LockResult returned by std::sync::Mutex::lock / RwLock::read / RwLock::write cascades the original panic across every other lock holder. Switch to tokio::sync::Mutex/RwLock or handle the poison via unwrap_or_else(|e| e.into_inner()).

## Why

See FastLED/fbuild#844. fbuild standardizes on internal bridge APIs
(`fbuild_core::http`, `fbuild_core::fs`, `fbuild_core::time`,
`fbuild_core::channel`, `fbuild_core::path`, `fbuild_cli::output`)
so the workspace has one source of truth for each external primitive.

## Allowlist

Empty by design. Bridge / scope exemptions live in `lib.rs` by file
path, not in `src/allowlist.txt`.

## Toolchain

Pinned to `nightly-2026-04-16` and Dylint 6.0.1, matching every other
Dylint library in this repository. See the top-level `dylints/README.md`.

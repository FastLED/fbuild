# `ban_std_fs_in_async`

Custom [dylint](https://github.com/trailofbits/dylint) for the fbuild
internal bridge sweep (FastLED/fbuild#844).

## What

Bans std::fs::* in crates/fbuild-daemon/src/** (Phase 1 scope; Phase 2 widens via HIR async-fn detection). Replacement: fbuild_core::fs::* (re-exports tokio::fs) or tokio::task::spawn_blocking.

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

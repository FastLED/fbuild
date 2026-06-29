# `ban_print_in_production`

Custom [dylint](https://github.com/trailofbits/dylint) for the fbuild
internal bridge sweep (FastLED/fbuild#844).

## What

Bans println!/eprintln!/print!/eprint! in crates/fbuild-cli/src/** and crates/fbuild-build/src/** (except crates/fbuild-cli/src/output.rs which IS the bridge). Replacement: fbuild_cli::output::{progress, result, warn, error, debug} or tracing::*.

## Why

See FastLED/fbuild#844. fbuild standardizes on internal bridge APIs
(`fbuild_core::http`, `fbuild_core::fs`, `fbuild_core::time`,
`fbuild_core::channel`, `fbuild_core::path`, `fbuild_cli::output`)
so the workspace has one source of truth for each external primitive.

## Allowlist

Empty by design. Bridge / scope exemptions live in `lib.rs` by file
path, not in `src/allowlist.txt`.

## Toolchain

Pinned to `nightly-2026-03-26` to match every other dylint in this
repo. See the top-level `dylints/README.md` for the full setup
instructions and the rationale for `build_dylint_driver.py`.

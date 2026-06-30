# `ban_env_var_set_after_import`

Custom [dylint](https://github.com/trailofbits/dylint) for the
initializer-order sweep (FastLED/fbuild#840).

## What

Bans `std::env::set_var` in production code outside the following
allow-listed scopes:

- `**/main.rs` (binary entry points)
- `**/src/bin/**` (alternate binary entry points)
- `**/tests/**` (integration tests)
- any module annotated `#[cfg(test)]`

## Why

Process-global env-var mutation has order-of-init hazards. The Python
fbuild hit exactly this bug: `paths.py` cached `FBUILD_DEV_MODE` at
module import time, but `cli.py` set the variable *after* that import
ran, so dev-mode paths silently resolved to prod (see the project
memory "Daemon paths.py import-time bug"). The Rust-side analog is a
module that reads `std::env::var` into a `static` / `OnceLock` before
another module ran `std::env::set_var`.

The convention this lint enforces: only `main()` (and tests, which set
up their own isolated process state) may mutate env vars. Every other
site should accept the value as an argument, read it lazily through a
function (not a static), or — for path resolution — go through
`fbuild-paths`'s lazy helpers.

## Limitation (convention-based, not dataflow)

This is a *call-site* check. The lint catches every `std::env::set_var`
outside the allow-list; it does NOT prove the variable was actually
read by another module before this call ran. A proper "set after read"
check requires MIR-level dataflow across the whole crate graph
("**MIR for completeness later**" — tracked as a follow-up on #840).
The convention is the fastest-landing prevention available today.

## Allowlist

Empty by design. Scope exemptions live in `lib.rs` by file path, not
in `src/allowlist.txt`.

## Toolchain

Pinned to `nightly-2026-03-26` to match every other dylint in this
repo. See the top-level `dylints/README.md` for the full setup
instructions and the rationale for `build_dylint_driver.py`.

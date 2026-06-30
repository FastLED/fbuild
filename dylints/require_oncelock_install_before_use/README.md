# `require_oncelock_install_before_use`

Custom [dylint](https://github.com/trailofbits/dylint) for the
initializer-order sweep (FastLED/fbuild#840).

## What

Bans `std::sync::OnceLock::set` and `OnceLock::set_blocking` in
production code outside the following allow-listed scopes:

- `**/main.rs` (binary entry points)
- `**/src/bin/**` (alternate binary entry points)
- `**/tests/**` (integration tests)
- any module annotated `#[cfg(test)]`

## Why

`OnceLock` is process-global single-assignment state. The only safe
way to install a value is from a tightly-controlled call site that
runs *before* any reader gets a chance — in practice, `main()` (or
test setup). Any other site implies the order-of-init bug class: a
reader has already filled the lock via `get_or_init` and the late
`set` silently no-ops, so the wrong value sticks for the process
lifetime.

This is the convention shortcut per #840's "Approaches to investigate
→ convention-based static check" recommendation. The motivating Python
analog was `paths.py` capturing `FBUILD_DEV_MODE` at module import
time, before `cli.py` mutated the variable. The Rust shape: someone
calls `MY_GLOBAL.set(...)` from a non-`main` module, but another
caller already populated the lock via `get_or_init`, so the late
`.set(...)` returns `Err` and is silently dropped.

Use `get_or_init` (lazy, idempotent), or install the value inside
`main()` before any module reads it.

## Limitation (convention-based, not dataflow)

This lint catches the *attempt* at the call site. It does NOT prove
`get_or_init` ran first or that another reader has already populated
the lock. A proper "was `.get_or_init` already called?" check requires
MIR-level dataflow across the whole crate graph and is out of scope
here ("**MIR for completeness later**" — tracked as a follow-up on
#840). The convention is the fastest-landing prevention available
today.

## Allowlist

Empty by design. Scope exemptions live in `lib.rs` by file path, not
in `src/allowlist.txt`.

## Toolchain

Pinned to `nightly-2026-03-26` to match every other dylint in this
repo. See the top-level `dylints/README.md` for the full setup
instructions and the rationale for `build_dylint_driver.py`.

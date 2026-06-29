# `ban_std_mpsc_in_async_reachable` — sources

See the top-level [`../README.md`](../README.md) for the lint contract,
allowlist policy, and replacement API. This directory contains:

- **`lib.rs`** — the late-pass `LintContext` visitor.
- **`allowlist.txt`** — empty by design for the #844 sweep. Bridge /
  scope exemptions live in `lib.rs` by file path.

Both files are loaded at lint-compile time via `include_str!` in
`lib.rs`. To regenerate after editing the allowlist bump the version
in this lint's `Cargo.toml` (the dylint .so cache is keyed off the
manifest, not the allowlist).

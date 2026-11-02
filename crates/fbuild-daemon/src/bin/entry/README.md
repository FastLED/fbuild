# Daemon entry point (library module, not a binary)

`mod.rs` holds [`fbuild_daemon::entry::run`] — the body that used to live in
`src/main.rs`. It is a **library module**, not a binary target: cargo only
auto-discovers `src/bin/*/main.rs`, and this directory has no `main.rs`.

## Why it lives under `src/bin/`

It is a program entry point, and two dylints scope themselves by path:

- `ban_process_exit_outside_main` — `std::process::exit` is allowed only in
  `**/src/main.rs` and `**/src/bin/**`;
- `ban_env_var_set_after_import` — `std::env::set_var` is allowed only in
  `**/main.rs`, `**/src/bin/**`, `#[cfg(test)]` and `tests/**`.

Both exemptions exist because those calls are only safe at process start,
before any module has read the environment. `run()` is called as the first
statement of the `fbuild-daemon` binary and, later, from `fbuild`'s `argv[0]`
multicall dispatch — also before clap, the tokio runtime, or any module-level
`OnceLock` exists (FastLED/fbuild#1851, #840, #826). Neither dylint allowlist
is touched: those files say new entries must not be added, and this is the
path-based exemption instead.

See also `../README.md` for the test-helper binaries in this directory.

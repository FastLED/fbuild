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

## `self_eviction.rs`

The self-eviction window predicate lives in its own module so it can be unit
tested with an explicit "now" instead of sleeping (FastLED/fbuild#2144).

`try_self_evict(ctx, &mut empty_since, now, timeout)` answers "has the daemon
been empty for the whole window?" using `DaemonContext::is_empty()` — the
admitted-request and active-operation counters plus serial sessions and pending
attaches, never the legacy `operation_in_progress` boolean, which one finishing
operation could clear while a sibling build was still running (FastLED/fbuild#51).
When the window expires it commits with `try_begin_shutdown(false)`, which closes
operation admission *before* the loop signals shutdown, and it re-arms the window
from the first quiet tick so a daemon that just finished a build gets a fresh
timeout.

`mod.rs` keeps the once-a-second loop (log bookkeeping, the 12-hour fallback,
stale-lock cleanup) and stays under the 1000-LOC gate by delegating this
predicate here.

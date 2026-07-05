# `dylints/`

Custom [dylint](https://github.com/trailofbits/dylint) lints for fbuild
production code. Each lint lives in its own crate so it can pin its own
nightly toolchain (the rustc internal API moves fast; the workspace
itself stays on stable 1.94.1).

## Crates

- **`ban_raw_subprocess/`** — forbids `Command::{spawn, output, status}`
  on `std::process::Command` and `tokio::process::Command` in production
  code (`crates/*/src/`). All subprocess spawns must flow through
  `fbuild_core::subprocess::run_command` /
  `fbuild_core::containment::*`. See #264.
- **`ban_std_pathbuf/`** — bans raw `std::path::PathBuf` in workspace
  code; steers callers at `fbuild_core::path::NormalizedPath` so paths
  carry the normalization invariant Windows requires. Legacy call sites
  exempted via `src/allowlist.txt`. See #826 / #436 / #437 / #282.
- **`ban_unrooted_tempdir/`** — bans `tempfile::tempdir()` /
  `tempfile::TempDir::new()` / `tempfile::NamedTempFile::new()` /
  `std::env::temp_dir()` in production code; steers callers at the
  `_in(...)` variants rooted under `fbuild_paths::get_cache_root()` so
  every byte fbuild writes lives under one user-visible directory.
  Legacy call sites exempted via `src/allowlist.txt`. See #826.
- **`ban_direct_serialport/`** — bans direct use of the `serialport`
  crate outside `crates/fbuild-serial/` and a small set of diagnostic
  CLI entry points. All serial access must flow through
  `fbuild-serial`'s blessed APIs so DTR/RTS rules, retry counts, and
  the Windows USB-CDC contract stay consistent. See #826.
- **`ban_file_based_locks/`** — bans file-based locking primitives
  (`OpenOptions::create_new(true)` lock-file pattern, `fs2::FileExt`,
  `flock`). All locking flows through the daemon's in-memory managers
  per the `CLAUDE.md` "no file-based locks" rule. Locks in the
  invariant; allowlist is empty. See #826.
- **`ban_deploy_tool_direct_invocation/`** — bans direct
  `Command::new("esptool" | "avrdude" | "picotool" | "dfu-util" |
  "pyocd")` invocations outside `crates/fbuild-deploy/`. All deploy-
  tool spawns must flow through `fbuild deploy`. See #826 / #694.
- **`ban_process_exit_outside_main/`** — bans `std::process::exit`
  outside `crates/*/src/main.rs` and `crates/*/src/bin/*.rs`. Skipping
  destructors leaks temp files, kills containment guards, and
  truncates in-flight HTTP/WS responses. Legacy CLI subcommand
  dispatchers exempted via `src/allowlist.txt`. See #826.
- **`ban_unwrap_in_production/`** — bans `.unwrap()` inside production
  code under `crates/fbuild-daemon/src/**/*.rs` and
  `crates/fbuild-cli/src/cli/**/*.rs` (tests exempt by sibling-file
  name — `tests.rs`, `*_tests.rs`, `tests_*.rs` — and by
  `#[cfg(test)]` module walking). PR #833 first landed this lint for
  the daemon-handler subdirectory; FastLED/fbuild#844 item 11
  widened it to all of `fbuild-daemon/src/` plus `fbuild-cli/src/cli/`
  and tightened sibling-test-file detection. New violations would
  crash the daemon or drop CLI users into a Rust backtrace. See #826
  and #844.
- **`cli_no_build_deploy_direct_use/`** — bans `fbuild_build::*` /
  `fbuild_deploy::*` references in `crates/fbuild-cli/src/` outside
  the diagnostic-subcommand allowlist. Enforces the "thin HTTP
  client" rule from `crates/CLAUDE.md` §"Dependency Graph". See
  #826.
- **`require_multi_thread_flavor_when_spawning/`** — flags
  `#[tokio::test]` (without `flavor = "multi_thread"`) on async fns
  that call `tokio::spawn(...)` in their body. The default
  `current_thread` flavor deadlocks any test that awaits cross-task
  state. Legacy violators exempted via `src/allowlist.txt`. See
  #826.
- **`ban_std_sync_mutex_in_async/`** — bans `std::sync::Mutex` /
  `std::sync::RwLock` *type uses* inside `crates/fbuild-daemon/src/`
  and `crates/fbuild-serial/src/` (tests exempt). Holding the guard
  across `.await` starves the Tokio worker and a panic poisons the
  lock. Existing synchronous uses exempted via `src/allowlist.txt`.
  See #826.

### FastLED/fbuild#844 bridge sweep (Phase 1 — APIs + lints land together)

- **`ban_bare_reqwest/`** — forbids `reqwest::Client::new()` /
  `reqwest::ClientBuilder::new()` / `reqwest::get` /
  `reqwest::blocking::get` outside the bridge module
  (`crates/fbuild-core/src/http.rs`). Use
  `fbuild_core::http::client()` /
  `fbuild_core::http::client_with_timeout(...)` /
  `fbuild_core::http::blocking_client(...)`. Zero allowlist.
- **`ban_std_fs_in_async/`** — forbids `std::fs::*` in
  `crates/fbuild-daemon/src/**` (Phase 1 scope; Phase 2 will widen
  via HIR async-fn detection). Use `fbuild_core::fs::*` (re-exports
  `tokio::fs`) or wrap in `tokio::task::spawn_blocking`. Zero
  allowlist.
- **`ban_tokio_fs_direct_import/`** — forbids `use tokio::fs[…]`
  outside `crates/fbuild-core/src/fs.rs`. Use `use fbuild_core::fs`
  instead so the workspace has one source of truth for the async
  filesystem surface. Zero allowlist.
- **`ban_std_thread_sleep/`** — forbids `std::thread::sleep` /
  `sleep_ms` in fbuild production code. Use
  `fbuild_core::time::sleep(...).await` (with the named
  `POLL_*` / `*_TIMEOUT` constants where applicable). Zero
  allowlist.
- **`ban_std_mpsc_in_async_reachable/`** — forbids any `std::sync::mpsc`
  item (channel, Sender, Receiver, SyncSender, sync_channel) in
  fbuild production code. Use `fbuild_core::channel::{bounded,
  unbounded}` (tokio mpsc) so `.recv().await` yields to the
  runtime. Zero allowlist.
- **`ban_tokio_mpsc_direct_import/`** — forbids `use tokio::sync::mpsc[…]`
  outside `crates/fbuild-core/src/channel.rs`. Use `use
  fbuild_core::channel` instead. Zero allowlist.
- **`ban_std_fs_canonicalize/`** — forbids `std::fs::canonicalize`
  and `tokio::fs::canonicalize` in fbuild production code. Use
  `fbuild_core::path::canonicalize_existing(...).await` — strips
  the Windows `\\?\` UNC prefix and returns a `NormalizedPath` with
  platform-stable Hash/Eq/Ord. Zero allowlist.
- **`ban_runtime_new_outside_main/`** — forbids
  `tokio::runtime::Runtime::new` and `Builder::new_*` in production
  code outside `main.rs`, `src/bin/*.rs`, `#[cfg(test)]` modules,
  and `tests/**`. Restructure to `async fn`, accept a
  `tokio::runtime::Handle`, or use `tokio::task::spawn_blocking`.
  Zero allowlist.
- **`ban_poison_panic/`** — flags `.unwrap()` / `.expect(...)` on
  `LockResult` returned by `std::sync::Mutex::lock` /
  `RwLock::read` / `RwLock::write`. Either switch to
  `tokio::sync::Mutex` / `RwLock` (no poison concept) or handle
  the poison: `.unwrap_or_else(|e| e.into_inner())`. Zero
  allowlist.
- **`ban_print_in_production/`** — forbids `println!` / `eprintln!`
  / `print!` / `eprint!` in `crates/fbuild-cli/src/**` and
  `crates/fbuild-build/src/**`, except `crates/fbuild-cli/src/output.rs`
  (the bridge itself). Use `fbuild_cli::output::{progress, result,
  warn, error, debug}` (tracing-backed) so `--quiet`,
  `--verbose`, and `--color={auto,always,never}` flow through one
  level filter. Bridge-only allowlist.

### FastLED/fbuild#911 — path-normalization anti-pattern

- **`ban_manual_slash_normalize/`** — forbids hand-rolled
  `.replace('\\', "/")` calls; steers callers at
  `fbuild_core::path::NormalizedPath::display_slash()`, which
  already owns the Windows `\` → `/` rewrite + UNC-prefix strip.
  Companion to `ban_std_pathbuf/`. The four bugs #875 / #885 /
  #890 / #912 were the same class — a path argument reached the
  compiler / linker with backslashes still in it — and each fix
  added yet another hand-rolled site. This lint closes the loop by
  making the anti-pattern impossible to introduce. Allowlist: the
  primitive itself (`fbuild-core/src/path.rs`), the DOT-string
  escape in the symbol-graph emitter, the glob-pattern helper in
  `source_scanner.rs`, and the lint's own UI fixture. See
  FastLED/fbuild#911.
- **`ban_raw_path_prefix_compare/`** — forbids raw
  `Path::{starts_with, strip_prefix}` in production code; steers
  callers at `fbuild_core::path::normalize_for_key` /
  `NormalizedPath`. These compare path components *as written*, so a
  canonicalized-vs-raw spelling (or `\\?\` prefix, trailing slash, or
  case difference) silently mismatches — which, when one side is a
  cache root or project dir, encodes the project directory into a
  cross-project cache key and the global cache never hits. Filed after
  exactly that defeated the framework core-artifact / fw-libs caches.
  Allowlist: the normalization bridge itself
  (`fbuild-core/src/path.rs`) and the blessed workspace relativization
  in `fbuild-build/src/zccache.rs`, plus same-normal-form call sites.
  See FastLED/fbuild#952 and `agents/docs/path-conventions.md`.

## Running locally

```bash
# One-time setup
rustup toolchain install nightly-2026-03-26 \
    --component llvm-tools-preview --component rust-src --component rustc-dev \
    --profile minimal
soldr cargo install cargo-dylint dylint-link --version 5.0.0 --locked
uv run python ci/build_dylint_driver.py   # builds a matching driver

# Run all dylints over the workspace
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:${PATH}"
cargo dylint --all -- --workspace --all-targets
```

CI runs this on every push/PR via `.github/workflows/dylint.yml`.

## Why a separate toolchain pin

`dylint_linting` builds against a specific nightly rustc; the rustc
internal API (`rustc_lint`, `rustc_hir`, `rustc_span`) changes between
nightlies. Keeping each dylint crate out of the stable workspace lets
it pin `nightly-2026-03-26` in its own `rust-toolchain.toml` without
forcing the entire workspace to nightly.

The workspace registers the lint directory via:

```toml
[workspace.metadata.dylint]
libraries = [{ path = "dylints/*" }]
```

so `cargo dylint --all` picks every dylint up automatically.

## Why `build_dylint_driver.py`

Published `dylint_driver` 5.0.0 doesn't compile against the
nightly-2026-03-26 toolchain (rustc internals drift). `cargo-dylint` would
try to build it from crates.io and fail with `E0609: no field
env_depinfo`. The script clones the dylint repo at the same git rev
`dylint_linting` is pinned to (`4bd91ce…`) and builds a matching driver
from that source, installing it where `cargo-dylint` expects.

This mirrors zccache's approach 1:1; the script is a direct port.

# `ban_raw_subprocess`

Custom [dylint](https://github.com/trailofbits/dylint) that forbids direct
calls to `Command::{spawn, output, status}` on `std::process::Command` and
`tokio::process::Command` in fbuild production code (anything under
`crates/*/src/`).

## Why

Every child process fbuild launches must flow through one of the blessed
wrappers in `crates/fbuild-core/src/`:

- `subprocess::run_command` — sync, captures stdout/stderr via
  `running-process-core::NativeProcess` so the drain loop can't deadlock
  on a full pipe buffer (see #141).
- `containment::spawn_contained` /
  `containment::tokio_spawn::spawn_contained` — applies Windows Job
  Object containment + Linux per-child pgid + originator-env propagation
  (see #129, #254).
- `containment::spawn_detached` — for the rare case where the child must
  outlive its launcher (daemon bootstrap from the CLI/Python).

Bypassing the wrappers silently regresses one or more of those
invariants. This lint catches both call shapes at compile time:

- Method-call: `cmd.spawn()` / `cmd.output()` / `cmd.status()`
- Qualified-path call:
  `std::process::Command::spawn(&mut cmd)` /
  `tokio::process::Command::output(&mut cmd)` /
  `<Command>::status(&mut cmd)`

## Scope

Only files whose path contains BOTH `crates/` and a subsequent `/src/`
segment are linted. Out of scope by design:

- `crates/*/tests/` — integration tests can spawn binaries under test
- `crates/*/examples/` — example code may spawn anything
- `crates/*/benches/` — benchmark harnesses
- `ci/` — Python tooling, not Rust production
- `dylints/` — this crate and its siblings
- Build scripts, anything else

## Allowlist

Files in scope that legitimately need raw spawns are listed in
`src/allowlist.txt`. Each entry needs an inline comment explaining why.
Current entries:

| Path | Reason |
|---|---|
| `crates/fbuild-core/src/subprocess.rs` | Internal helpers in the wrapper itself |
| `crates/fbuild-core/src/containment.rs` | IS the wrapper — `command.spawn()` is the implementation |
| `crates/fbuild-daemon/src/bin/containment_harness.rs` | Test harness for #129 |
| `crates/fbuild-cli/src/daemon_client.rs` | Daemon bootstrap — must outlive CLI |
| `crates/fbuild-python/src/daemon.rs` | Daemon bootstrap from Python interop |
| `crates/fbuild-build/src/zccache.rs` | Starts the zccache daemon (cross-tool) |
| `crates/fbuild-cli/src/cli/clang_tools.rs` | Async fan-out, no daemon containment in CLI |

## Toolchain

Pinned to the same `nightly-2026-03-26` channel and the same
`trailofbits/dylint` git rev (`4bd91ce…`) that zccache uses for its own
dylints — this keeps the `dylint_linting` / `dylint_driver` versions
aligned across both repos and lets `ci/build_dylint_driver.py` (ported
from zccache) build a matching driver for CI.

## Running locally

```bash
# One-time setup
rustup toolchain install nightly-2026-03-26 --component llvm-tools-preview \
    --component rust-src --component rustc-dev --profile minimal
soldr cargo install cargo-dylint dylint-link --version 5.0.0
uv run python ci/build_dylint_driver.py   # exports DYLINT_DRIVER_PATH

# Run the lint over the workspace
soldr cargo dylint --all -- --workspace --all-targets
```

CI runs this on every push/PR via `.github/workflows/dylint.yml`.

## See also

- Issue #264 — this lint's tracking issue (3 CR blockers from PR #262)
- PR #262 — original LTO fix that prototyped this lint and deferred it
- zccache `dylints/ban_raw_subprocess_in_daemon/` — sibling pattern this
  is modeled on
- `ci/find_direct_subprocess.py` — the prior string-matching guard that
  catches `Command::new(`; complements this lint by checking the
  constructor side at the import/syntax level

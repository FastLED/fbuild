# `ban_raw_subprocess`

Custom [dylint](https://github.com/trailofbits/dylint) that forbids direct
calls to `Command::{spawn, output, status}` on `std::process::Command` and
`tokio::process::Command` in fbuild production code (anything under
`crates/*/src/`).

## Why

Every child process fbuild launches must flow through one of the blessed
wrappers in `crates/fbuild-core/src/`:

- `subprocess::run_command` — sync, captures stdout/stderr via
  `running-process::NativeProcess` so the drain loop can't deadlock
  on a full pipe buffer (see #141).
- `platform::process::spawn_contained` /
  `platform::process::spawn_tokio_contained` — applies native containment,
  kill-on-drop, and originator-env propagation
  (see #129, #254).
- `platform::process::spawn_detached` — for the rare case where the child must
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
| `crates/fbuild-core/src/platform/macos/process.rs` | Selected implementation uses `/bin/ps` for PID image inspection |
| `crates/fbuild-daemon/src/bin/containment_harness.rs` | Test harness for #129 |
| `crates/fbuild-build-engine/src/zccache.rs` | Starts the zccache daemon (cross-tool) |
| `crates/fbuild-cli/src/cli/clang_tools.rs` | Async fan-out, no daemon containment in CLI |

## Toolchain

Pinned to `nightly-2026-04-16` and Dylint 6.0.1, matching every other
Dylint library in this repository.

## Running locally

```bash
# One-time setup
soldr rustup toolchain install nightly-2026-04-16 --component llvm-tools-preview \
    --component rust-src --component rustc-dev --component rustfmt --profile minimal
soldr cargo install cargo-dylint dylint-link --version 6.0.1

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

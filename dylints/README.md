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

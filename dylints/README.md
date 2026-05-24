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
nightlies. Keeping the dylint crate in `[workspace.exclude]` lets it
pin `nightly-2026-03-26` in its own `rust-toolchain.toml` without
forcing the entire workspace to nightly.

The workspace registers the lint directory via:

```toml
[workspace.metadata.dylint]
libraries = [{ path = "dylints/*" }]
```

so `cargo dylint --all` picks it up automatically.

## Why `build_dylint_driver.py`

Published `dylint_driver` 5.0.0 doesn't compile against the
nightly-2026-03-26 toolchain (rustc internals drift). `cargo-dylint` would
try to build it from crates.io and fail with `E0609: no field
env_depinfo`. The script clones the dylint repo at the same git rev
`dylint_linting` is pinned to (`4bd91ce…`) and builds a matching driver
from that source, installing it where `cargo-dylint` expects.

This mirrors zccache's approach 1:1; the script is a direct port.

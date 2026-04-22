# Dev Tools

Pip-installable package that provides repo-local development helpers and the `soldr` dependency.

Rust tooling should be invoked directly through [soldr](https://github.com/zackees/soldr), either as `soldr cargo ...` when soldr is on PATH or `uv run soldr cargo ...` through this repo-local environment. soldr uses `rustup which` to pick the right toolchain. The standard Cargo path is `soldr cargo ...`, so soldr's managed zccache path is enabled by default for repo Rust builds without repo-specific `RUSTC_WRAPPER` wiring.

## Contents

- **`pyproject.toml`** -- Declares the `soldr>=0.7.4` dependency and defines helper entry points: `run_fbuild`, `run_fbuild_daemon`, `publish`

# Dev Tools

Pip-installable package that provides repo-local development helper entry points.

Rust tooling is invoked through a **globally-installed** [soldr](https://github.com/zackees/soldr) (e.g. `uv tool install soldr`); `soldr` is no longer pulled into the repo-local `uv` environment as a dependency. soldr uses `rustup which` to pick the right toolchain. The standard Cargo path is `soldr cargo ...`, so soldr's managed zccache path is enabled by default for repo Rust builds without repo-specific `RUSTC_WRAPPER` wiring.

## Contents

- **`pyproject.toml`** -- Empty dependency list (soldr is global; see issue #251) and helper entry points: `run_fbuild`, `run_fbuild_daemon`, `publish`

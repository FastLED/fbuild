# Dev Tools

Pip-installable package that registers Rust toolchain trampolines as console scripts, so `uv run cargo` resolves to the rustup-managed toolchain without polluting global installs.

The trampolines route through [soldr](https://github.com/zackees/soldr) (pulled in via the `soldr>=0.7.0` dependency), which uses `rustup which` to pick the right toolchain. The standard Cargo path is `soldr cargo ...`, matching soldr's integration guidance without repo-specific `RUSTC_WRAPPER` wiring.

## Contents

- **`pyproject.toml`** -- Declares the `soldr>=0.7.0` dependency and defines console script entry points: `cargo`, `rustc`, `rustfmt`, `clippy-driver`, `run_fbuild`, `run_fbuild_daemon`, `publish`

# Dev Tools

Pip-installable package that registers Rust toolchain trampolines as console scripts, so `uv run cargo` resolves to the rustup-managed toolchain without polluting global installs.

The trampolines route through [soldr](https://github.com/zackees/soldr) (pulled in via the `soldr>=0.7.0` dependency), which uses `rustup which` to pick the right toolchain. `cargo` is invoked with `--no-cache` so the previous bare-cargo semantics are preserved — no RUSTC_WRAPPER is inserted.

## Contents

- **`pyproject.toml`** -- Declares the `soldr>=0.7.0` dependency and defines console script entry points: `cargo`, `rustc`, `rustfmt`, `clippy-driver`, `run_fbuild`, `run_fbuild_daemon`, `publish`

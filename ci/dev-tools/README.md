# Dev Tools

Pip-installable package that registers Rust toolchain trampolines as console scripts, so `uv run cargo` resolves to the rustup-managed toolchain without polluting global installs.

## Contents

- **`pyproject.toml`** -- Defines console script entry points: `cargo`, `rustc`, `rustfmt`, `clippy-driver`, `run_fbuild`, `run_fbuild_daemon`, `publish`

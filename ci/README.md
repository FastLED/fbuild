# CI & Development Tools

Python scripts for development tooling, invoked via `uv run`.

## Top-level scripts

- **`uv run cargo`** — Execute cargo with the correct Rust toolchain (trampoline)
- **`./lint`** — Workspace linting (rustfmt + clippy), supports single-file mode
- **`./test`** — Workspace tests, supports per-crate filtering

## Hooks (`ci/hooks/`)

Claude Code hooks that enforce project conventions:

- **tool_guard.py** — PreToolUse: blocks bare `cargo`/`rustc` (must use `uv run`) and bare `python`/`pip` (must use `uv`)
- **lint.py** — PostToolUse: auto-formats + runs clippy on edited `.rs` files
- **readme_guard.py** — PostToolUse: ensures every directory has a `README.md`

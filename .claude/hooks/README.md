# Claude Code Hooks

Hook scripts that enforce project standards automatically.

- **tool_guard.py** — PreToolUse: blocks bare Rust commands (must use `uv run`)
- **lint.py** — PostToolUse: auto-formats + runs clippy on edited .rs files
- **readme_guard.py** — PostToolUse: ensures every directory has a README.md
- **check-on-start.py** — SessionStart: captures git fingerprint
- **check-on-stop.py** — Stop: runs full workspace lint + tests

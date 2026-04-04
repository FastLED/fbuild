# Claude Code Hooks

Python scripts invoked by Claude Code lifecycle hooks, configured in `.claude/settings.json`. All executed via `uv run`.

## Contents

- **`board_context.py`** -- UserPromptSubmit: detects board-related prompts (board names, MCU keywords, board error patterns) and injects guidance about the `/board-support` skill, relevant commands, and external board registries
- **`tool_guard.py`** -- PreToolUse: blocks bare `cargo`/`rustc`/`rustfmt` and bare `python`/`pip` commands, requiring `uv run` or `_cargo`/`_rustc`/`_rustfmt` trampolines
- **`lint.py`** -- PostToolUse: runs per-file rustfmt + clippy on edited `.rs` files
- **`readme_guard.py`** -- PostToolUse: ensures every directory containing edited files has a `README.md`
- **`check-on-start.py`** -- SessionStart: captures a git fingerprint so the stop hook can detect changes
- **`check-on-stop.py`** -- Stop: runs full workspace lint + tests if files changed during the session

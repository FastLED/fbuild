# Claude Code Hooks

Python scripts invoked by Claude Code lifecycle hooks, configured in `.claude/settings.json`. All executed via `uv run`.

## Contents

- **`board_context.py`** -- UserPromptSubmit: detects board-related prompts (board names, MCU keywords, board error patterns) and injects guidance about the `/board-support` skill, relevant commands, and external board registries
- **`tool_guard.py`** -- PreToolUse: blocks bare `cargo`/`rustc`/`rustfmt`, legacy `uv run cargo`-style shims, `uv run soldr ...` (since soldr is no longer a venv dep — issue #251), and bare `python`/`pip` commands across shell tool variants such as Bash and PowerShell. Requires a globally-installed `soldr ...` for Rust tooling and `uv run` / `uv pip` for Python tooling.
- **`worktree_guard.py`** -- PreToolUse on the `Agent` tool: refuses `Agent` invocations with `isolation: "worktree"` when the current working directory is already inside a `.claude/worktrees/...` path. Prevents the recursive nesting (`.claude/worktrees/<a>/.claude/worktrees/<b>/...`) that was the root trigger of issue #481 (Windows MAX_PATH C1081 build failures + cc-rs PATH dumps overwhelming the context window).
- **`lint.py`** -- PostToolUse: runs per-file rustfmt + clippy on edited `.rs` files
- **`readme_guard.py`** -- PostToolUse: ensures every directory containing edited files has a `README.md`
- **`check-on-start.py`** -- SessionStart: captures a git fingerprint so the stop hook can detect changes
- **`check-on-stop.py`** -- Stop: runs full workspace lint + tests if files changed during the session
- **`_output.py`** -- shared `truncate_output(text, max_lines)` helper used by `lint.py` and `check-on-stop.py` to bound subprocess stderr/stdout fed back to Claude (prevents cc-rs build-script PATH dumps and Windows MAX_PATH C1081 errors from overflowing the context window)

# Claude Code Configuration

Project-level Claude Code settings, hooks, and rules for the fbuild workspace.

## Contents

- **`settings.json`** -- Hook configuration mapping lifecycle events (PreToolUse, PostToolUse, SessionStart, Stop) to scripts in `ci/hooks/`
- **`hooks/`** -- Reserved directory for hook scripts (currently empty; all hooks live in `ci/hooks/` and are referenced by `settings.json`)
- **`rules/`** -- Path-scoped and global rules loaded by Claude Code on demand

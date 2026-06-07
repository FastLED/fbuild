#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse hook: refuse Agent calls that would stack a worktree inside an
existing `.claude/worktrees/` directory.

The Claude Code Agent tool, when invoked with ``isolation: "worktree"``,
creates the worktree relative to the agent's current working directory. If
the calling agent is itself running inside ``.claude/worktrees/<parent>/``,
the new worktree lands at ``.claude/worktrees/<parent>/.claude/worktrees/
<child>/`` — and so on, recursively. This nesting was the root trigger of
issue #481 (Windows MAX_PATH C1081 + cc-rs PATH dumps overflowing the
1M context window).

This hook detects the nested-spawn case and denies it with a message
pointing at the fix: spawn the agent from the repo root (not from inside
another worktree), or omit ``isolation: "worktree"`` so it shares the
parent worktree.

Exit codes:
  0 - Always (deny is communicated via JSON ``hookSpecificOutput``).
"""

import json
import os
import sys
from pathlib import PurePosixPath


WORKTREE_SEGMENT = (".claude", "worktrees")


def _normalize_parts(path: str) -> tuple[str, ...]:
    """Split a path into segments, normalising backslashes to forward."""
    posix = PurePosixPath(path.replace("\\", "/"))
    return tuple(part for part in posix.parts if part not in ("", "/"))


def is_inside_worktree(cwd: str) -> bool:
    """True if ``cwd`` contains ``.claude/worktrees/<something>`` as a path
    segment pair — i.e. the agent is already running inside a worktree the
    harness previously created."""
    parts = _normalize_parts(cwd)
    for i in range(len(parts) - 2):
        if parts[i] == WORKTREE_SEGMENT[0] and parts[i + 1] == WORKTREE_SEGMENT[1]:
            return True
    return False


def should_deny(tool_name: str, tool_input: object, cwd: str) -> bool:
    """Deny only the case that produces nesting: Agent + worktree isolation
    spawned from inside an existing worktree."""
    if tool_name != "Agent":
        return False
    if not isinstance(tool_input, dict):
        return False
    if tool_input.get("isolation") != "worktree":
        return False
    return is_inside_worktree(cwd)


DENY_REASON = (
    "Refusing to spawn an Agent worktree from inside an existing worktree. "
    "Doing so creates `.claude/worktrees/<parent>/.claude/worktrees/<child>/` "
    "nesting, which has triggered Windows MAX_PATH C1081 build-script failures "
    "and overwhelmed Claude's context window (issue #481). "
    "Either omit `isolation: \"worktree\"` so the sub-agent shares the current "
    "worktree, or spawn the agent from the repo root instead of from inside "
    "a worktree."
)


def deny(reason: str) -> None:
    json.dump({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }, sys.stdout)


def main() -> None:
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        sys.exit(0)

    tool_name = data.get("tool_name", "")
    tool_input = data.get("tool_input", {})
    cwd = data.get("cwd") or os.getcwd()

    if should_deny(tool_name, tool_input, cwd):
        deny(DENY_REASON)

    sys.exit(0)


if __name__ == "__main__":
    main()

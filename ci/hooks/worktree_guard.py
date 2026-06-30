#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse hook: deny Agent tool calls with isolation="worktree"
when the spawning agent is already running inside `.claude/worktrees/`.

FastLED/fbuild#485 — Claude Code's Agent tool creates the new git
worktree relative to the calling agent's current working directory.
When the calling agent is itself running inside a worktree the harness
previously created, the new worktree lands at:

    .claude/worktrees/<parent>/.claude/worktrees/<child>/

Each additional sub-agent compounds the nesting. On Windows that path
crosses MAX_PATH (260 chars) inside a couple of source files and
breaks cargo build scripts (notably `cc-rs` compiling `bzip2-sys` /
`libz-sys`) with `fatal error C1081: 'file name too long'` — and the
retry loop floods stderr with kilobyte-sized PATH dumps, which can
overflow the session's context window (see #481, #482).

Until the harness anchors `isolation: "worktree"` at the repo root
(`git rev-parse --show-toplevel`) rather than the calling agent's
cwd, this hook refuses the Agent call locally and surfaces an
actionable error so the user can spawn the sub-agent without
`isolation: "worktree"` (or from the main checkout).

Exit codes:
  0 - Allow (writes a deny JSON via stdout if a violation is detected).
"""

from __future__ import annotations

import json
import os
import re
import sys

# Matches `.claude/worktrees/<branch>` at any depth in a POSIX-normalized
# path. We use this against the spawning session's cwd, so a single
# match means we're already inside a worktree.
WORKTREE_PATH_RE = re.compile(r"(?:^|/)\.claude/worktrees/[^/]+(?:/|$)")


def deny(reason: str) -> None:
    """Emit a deny verdict for the PreToolUse hook."""
    json.dump(
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        },
        sys.stdout,
    )


def normalize_path(path: str) -> str:
    """Return a POSIX-style absolute path with forward slashes."""
    if not path:
        return ""
    return path.replace("\\", "/")


def is_inside_worktree(cwd: str) -> bool:
    """True if `cwd` contains a `.claude/worktrees/<name>/` segment."""
    if not cwd:
        return False
    posix = normalize_path(cwd)
    return WORKTREE_PATH_RE.search(posix) is not None


def session_cwd(data: dict) -> str:
    """Best-effort extraction of the spawning session's cwd.

    Claude Code's PreToolUse payload includes `cwd` on most surfaces.
    Fall back to `$CLAUDE_PROJECT_DIR` if absent (the harness pins
    that to the main checkout's path; if the env var differs from
    `git rev-parse --show-toplevel` we're inside a worktree).
    """
    cwd = data.get("cwd")
    if isinstance(cwd, str) and cwd:
        return cwd
    env_cwd = os.environ.get("CLAUDE_PROJECT_DIR")
    if env_cwd:
        return env_cwd
    return os.getcwd()


def requests_worktree_isolation(tool_input: object) -> bool:
    """True iff this Agent call requests `isolation: "worktree"`."""
    if not isinstance(tool_input, dict):
        return False
    isolation = tool_input.get("isolation")
    return isinstance(isolation, str) and isolation.strip().lower() == "worktree"


def main() -> None:
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        sys.exit(0)

    if data.get("tool_name", "") != "Agent":
        sys.exit(0)

    tool_input = data.get("tool_input")
    if not requests_worktree_isolation(tool_input):
        sys.exit(0)

    cwd = session_cwd(data)
    if not is_inside_worktree(cwd):
        sys.exit(0)

    deny(
        "FastLED/fbuild#485: this session is already running inside a "
        f"`.claude/worktrees/` checkout ({cwd}). Spawning an Agent with "
        "`isolation: \"worktree\"` from here would stack a new worktree "
        "INSIDE the parent worktree, producing paths long enough to "
        "trip Windows MAX_PATH and break cargo build scripts (`cc-rs` "
        "for `bzip2-sys`/`libz-sys`). Re-spawn the Agent without "
        "`isolation` (the harness will fall back to in-place execution), "
        "or run the slash command from the main checkout instead. "
        "Track the upstream harness fix at "
        "https://github.com/FastLED/fbuild/issues/485."
    )
    sys.exit(0)


if __name__ == "__main__":
    main()

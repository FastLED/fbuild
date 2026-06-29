#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse hook: forbid creating new Rust crates via Edit/Write.

fbuild is intentionally kept close to a monocrate (see CLAUDE.md and
FastLED/fbuild#560). New functionality is folded into an existing crate as a
*module*, not introduced as a brand-new crate. The CI check at
`ci/check_workspace_crates.py` enforces this in batch on every PR; this
hook enforces it in real time by blocking any attempt to write a `Cargo.toml`
at a path that is not already part of the approved set.

A standalone Cargo project anywhere in the repo (workspace member or
otherwise) is treated as a new crate. The only allowed `Cargo.toml` writes
are:
  - the workspace root `Cargo.toml`,
  - one of the approved member directories,
  - one of the approved excluded directories (dylints/*).

A genuinely-justified new crate requires editing both `APPROVED_CRATE_DIRS`
in this file AND `APPROVED_MEMBERS` in `ci/check_workspace_crates.py` in
the same PR, with maintainer-reviewed rationale in the PR body.

Exit codes:
  0 — allow (writes a deny JSON via stdout if a violation is detected).
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path, PurePosixPath


# Directories that are allowed to contain a `Cargo.toml`. Keep this list
# in sync with the `[workspace] members` + `exclude` lists in the root
# `Cargo.toml` and with `APPROVED_MEMBERS` in
# `ci/check_workspace_crates.py`. Use POSIX-style relative paths.
APPROVED_CRATE_DIRS: frozenset[str] = frozenset(
    {
        # Repo root workspace manifest:
        "",
        # Workspace members:
        "crates/fbuild-core",
        "crates/fbuild-config",
        "crates/fbuild-paths",
        "crates/fbuild-packages",
        "crates/fbuild-serial",
        "crates/fbuild-build",
        "crates/fbuild-deploy",
        "crates/fbuild-daemon",
        "crates/fbuild-cli",
        "crates/fbuild-python",
        "crates/fbuild-test-support",
        "crates/fbuild-header-scan",
        "crates/fbuild-library-select",
        "bench/fastled-examples",
        # Workspace-excluded crates with their own toolchains:
        "dylints/ban_raw_subprocess",
        "dylints/ban_std_pathbuf",
        "dylints/ban_unrooted_tempdir",
        "dylints/ban_direct_serialport",
        "dylints/ban_file_based_locks",
        "dylints/ban_deploy_tool_direct_invocation",
        "dylints/ban_process_exit_outside_main",
        "dylints/ban_unwrap_in_daemon_handlers",
        "dylints/cli_no_build_deploy_direct_use",
        "dylints/require_multi_thread_flavor_when_spawning",
        "dylints/ban_std_sync_mutex_in_async",
    }
)


def repo_root() -> Path:
    """The git toplevel, since the hook is launched from there."""
    return Path(os.getcwd()).resolve()


def deny(reason: str) -> None:
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


def relative_dir(file_path: str) -> str | None:
    """Return the repo-relative POSIX directory of `file_path`, or None
    if the path is outside the repo (e.g. a temp file)."""
    try:
        abs_path = Path(file_path).resolve()
        rel = abs_path.relative_to(repo_root())
    except (ValueError, OSError):
        return None
    parent = rel.parent
    # `Path('.').as_posix()` -> '.', normalize the root manifest case to '':
    posix = PurePosixPath(parent).as_posix()
    posix = _strip_worktree_prefix(posix)
    return "" if posix == "." else posix


# `/clud-pr` (and related skills) work inside disposable worktrees under
# `.claude/worktrees/<branch>/`. The harness pins `$CLAUDE_PROJECT_DIR`
# to the **main** checkout even when the edit target is inside a
# worktree, so the path that reaches this hook looks like
# `.claude/worktrees/<branch>/crates/fbuild-build/Cargo.toml`. Without
# this prefix-strip, the same allowlist check that passes on a direct
# `crates/fbuild-build/Cargo.toml` edit fails inside a worktree — which
# is a bug, not a policy: a worktree of this repo is still this repo
# with the same approved-crates contract. See FastLED/fbuild#794.
_WORKTREE_PREFIX_RE = re.compile(r"^\.claude/worktrees/[^/]+(?:/|$)")


def _strip_worktree_prefix(posix: str) -> str:
    """Strip a leading `.claude/worktrees/<branch>/` if present."""
    stripped = _WORKTREE_PREFIX_RE.sub("", posix, count=1)
    return stripped or "."


def is_cargo_toml(file_path: str) -> bool:
    """True if the path ends in `Cargo.toml` (case-insensitive on Windows
    but exact on POSIX). Catches the canonical filename used by Cargo to
    mark a crate root."""
    name = Path(file_path).name
    if sys.platform.startswith("win"):
        return name.lower() == "cargo.toml"
    return name == "Cargo.toml"


def extract_file_path(data: dict) -> str:
    tool_input = data.get("tool_input")
    if not isinstance(tool_input, dict):
        return ""
    value = tool_input.get("file_path")
    if isinstance(value, str):
        return value.strip()
    return ""


# A future-proofing belt-and-suspenders check: if someone tries to
# Write/Edit something that *looks* like a Cargo project root but uses a
# non-standard name (`cargo.toml`, weird casing on Linux), the filename
# regex below catches it too. Keep it tight — we only want to flag actual
# Cargo manifest filenames.
CARGO_TOML_RE = re.compile(r"^[Cc]argo\.toml$")


def main() -> None:
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        sys.exit(0)

    tool_name = data.get("tool_name", "")
    if tool_name not in {"Edit", "Write", "NotebookEdit"}:
        sys.exit(0)

    file_path = extract_file_path(data)
    if not file_path:
        sys.exit(0)

    if not (is_cargo_toml(file_path) or CARGO_TOML_RE.match(Path(file_path).name)):
        sys.exit(0)

    rel_dir = relative_dir(file_path)
    if rel_dir is None:
        # Outside the repo — let it through (e.g. tempfile).
        sys.exit(0)

    if rel_dir in APPROVED_CRATE_DIRS:
        sys.exit(0)

    deny(
        f"Refusing to create/modify Cargo.toml at '{rel_dir or '.'}': "
        "fbuild is kept close to a monocrate (see CLAUDE.md and "
        "FastLED/fbuild#560). New functionality must be folded into one of "
        "the existing crates as a module, not introduced as a brand-new "
        "crate. The approved Cargo project directories are: "
        f"{sorted(APPROVED_CRATE_DIRS - {''}) }, plus the workspace root. "
        "If a new crate is genuinely justified, update "
        "`APPROVED_CRATE_DIRS` in `ci/hooks/crate_guard.py` AND "
        "`APPROVED_MEMBERS` in `ci/check_workspace_crates.py` in the same "
        "PR, with maintainer-reviewed rationale in the PR body."
    )
    sys.exit(0)


if __name__ == "__main__":
    main()

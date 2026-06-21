#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Create a `git worktree` for the `www` orphan branch, bootstrapping it
empty if the branch does not yet exist on the remote.

Mirrors the inline shell that previously lived in update-data.yml. Keeping it
in Python makes the orphan-bootstrap logic unit-testable (`tests/test_setup_www_worktree.py`)
and the YAML caller becomes a single `uv run` line.

Run via:
    setup_www_worktree.py --worktree .www --branch www --remote origin
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path


def _run(*args: str, cwd: Path | None = None, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=cwd,
        text=True,
        capture_output=True,
        check=check,
    )


def remote_has_branch(remote: str, branch: str, *, runner=_run) -> bool:
    """Return True iff `<remote>/<branch>` exists on the remote.

    Note: the caller's CWD must be inside the repo that defines `remote`.
    The GH Actions workflow runs this from the checkout root, so `origin`
    resolves to the fbuild repo. Tests substitute a fake runner so CWD
    doesn't matter for them.
    """
    cp = runner("git", "ls-remote", "--heads", remote, branch, check=False)
    if cp.returncode != 0:
        # ls-remote against an unreachable remote returns non-zero; surface it.
        raise RuntimeError(
            f"git ls-remote {remote} {branch} failed (rc={cp.returncode}): {cp.stderr.strip()}"
        )
    return bool(cp.stdout.strip())


def setup(
    *,
    worktree: Path,
    branch: str,
    remote: str = "origin",
    runner=_run,
) -> str:
    """Materialize `worktree` as a worktree for `branch`.

    Returns a one-line status string:
      - "fetched": branch existed on remote, worktree checked out at branch tip.
      - "bootstrapped": branch did not exist; worktree initialized empty.
    """
    worktree.parent.mkdir(parents=True, exist_ok=True)
    if remote_has_branch(remote, branch, runner=runner):
        runner("git", "fetch", remote, f"{branch}:{branch}")
        runner("git", "worktree", "add", str(worktree), branch)
        return "fetched"
    # Branch missing → orphan worktree.
    runner("git", "worktree", "add", "--detach", str(worktree))
    runner("git", "checkout", "--orphan", branch, cwd=worktree)
    # `git rm -rf .` may fail with "did not match any files" on a truly empty
    # tree; tolerate that.
    runner("git", "rm", "-rf", ".", cwd=worktree, check=False)
    return "bootstrapped"


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--worktree", required=True, type=Path)
    p.add_argument("--branch",   required=True)
    p.add_argument("--remote",   default="origin")
    args = p.parse_args()
    state = setup(worktree=args.worktree, branch=args.branch, remote=args.remote)
    print(f"{args.branch}@{args.worktree}: {state}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

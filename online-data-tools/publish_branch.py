#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Commit + history-prune + push a worktree.

Single Python entry-point replacing what was previously three YAML steps per
branch (commit, prune, push). The orphan branches (`online-data`, `www`) both
cap history at 200 commits and force-with-lease push after a rewrite.

First-time push: falls back to a plain push when the remote ref does not
yet exist (no ref to lease against).

Run via:
    publish_branch.py --worktree .www --branch www --remote origin \\
                      --message "chore(www): nightly refresh" \\
                      --history-limit 200 [--dry-run]
"""

from __future__ import annotations

import argparse
import datetime as _dt
import os
import subprocess
import sys
from pathlib import Path
from typing import Callable

Runner = Callable[..., subprocess.CompletedProcess[str]]


def _run(*args: str, cwd: Path | None = None, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, text=True, capture_output=True, check=check)


def _today_utc() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%d")


def stage_all(worktree: Path, *, runner: Runner = _run) -> None:
    runner("git", "add", "-A", cwd=worktree)


def has_staged_changes(worktree: Path, *, runner: Runner = _run) -> bool:
    cp = runner("git", "diff", "--cached", "--quiet", cwd=worktree, check=False)
    # `git diff --cached --quiet` exits 1 iff there are staged differences.
    return cp.returncode != 0


def commit(
    worktree: Path,
    *,
    title: str,
    body: str | None = None,
    runner: Runner = _run,
) -> None:
    args = ["git", "commit", "-m", title]
    if body:
        args += ["-m", body]
    runner(*args, cwd=worktree)


def history_length(worktree: Path, *, runner: Runner = _run) -> int:
    cp = runner("git", "rev-list", "--count", "HEAD", cwd=worktree)
    return int(cp.stdout.strip())


def prune_history(
    worktree: Path,
    *,
    limit: int,
    runner: Runner = _run,
) -> bool:
    """Truncate `worktree`'s HEAD to at most `limit` commits.

    Uses `git replace --graft` + `git filter-repo` (the GitHub-hosted Ubuntu
    runner ships filter-repo). Returns True if a rewrite happened, False if
    history was already short enough.
    """
    if limit < 1:
        raise ValueError(f"--history-limit must be >= 1, got {limit}")
    total = history_length(worktree, runner=runner)
    if total <= limit:
        return False
    cp = runner("git", "rev-list", f"--max-count={limit}", "HEAD", cwd=worktree)
    last = cp.stdout.strip().splitlines()[-1]
    runner("git", "replace", "--graft", last, cwd=worktree)
    runner("pip", "install", "--quiet", "git-filter-repo")
    runner("git", "filter-repo", "--force", "--refs", "HEAD", cwd=worktree)
    # Drop the synthetic refs/replace entries created by the graft.
    cp = runner("git", "for-each-ref", "--format=delete %(refname)",
                "refs/replace/", cwd=worktree)
    if cp.stdout.strip():
        proc = subprocess.run(
            ["git", "update-ref", "--stdin"],
            cwd=worktree, input=cp.stdout, text=True, check=True,
        )
        del proc
    return True


def remote_has_branch(
    worktree: Path,
    *,
    remote: str,
    branch: str,
    runner: Runner = _run,
) -> bool:
    # cwd matters — `origin` is a per-repo alias, not a global. Without it
    # the query resolves against whichever repo holds the process's CWD,
    # producing wrong-answer-by-accident bugs.
    cp = runner("git", "ls-remote", "--heads", remote, branch,
                cwd=worktree, check=False)
    if cp.returncode != 0:
        raise RuntimeError(
            f"git ls-remote {remote} {branch} failed: {cp.stderr.strip()}"
        )
    return bool(cp.stdout.strip())


def push(
    worktree: Path,
    *,
    remote: str,
    branch: str,
    runner: Runner = _run,
) -> str:
    """Push HEAD to `<remote>/<branch>`.

    Uses `--force-with-lease` when the remote ref exists; otherwise a plain
    push (no ref to lease against). Returns "force-with-lease" or "plain".
    """
    if remote_has_branch(worktree, remote=remote, branch=branch, runner=runner):
        runner("git", "push", "--force-with-lease", remote, f"HEAD:{branch}", cwd=worktree)
        return "force-with-lease"
    runner("git", "push", remote, f"HEAD:{branch}", cwd=worktree)
    return "plain"


def publish(
    *,
    worktree: Path,
    remote: str,
    branch: str,
    message: str,
    body: str | None = None,
    history_limit: int,
    dry_run: bool = False,
    runner: Runner = _run,
) -> dict:
    """End-to-end: stage, commit (if changes), prune, push.

    Returns a summary dict so the caller (or test) can assert on it without
    parsing stdout.
    """
    out: dict = {"worktree": str(worktree), "branch": branch}
    stage_all(worktree, runner=runner)
    if not has_staged_changes(worktree, runner=runner):
        out["changed"] = False
        return out
    out["changed"] = True
    if dry_run:
        out["dry_run"] = True
        return out
    commit(worktree, title=f"{message} {_today_utc()}", body=body, runner=runner)
    out["pruned"] = prune_history(worktree, limit=history_limit, runner=runner)
    out["push"]   = push(worktree, remote=remote, branch=branch, runner=runner)
    return out


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--worktree",       required=True, type=Path)
    p.add_argument("--branch",         required=True)
    p.add_argument("--remote",         default="origin")
    p.add_argument("--message",        required=True,
                   help="Commit-title prefix; today's UTC date is appended.")
    p.add_argument("--body",           default=None)
    p.add_argument("--history-limit",  type=int, default=200)
    p.add_argument("--dry-run",        action="store_true",
                   help="Stage + check for changes but do not commit/push.")
    args = p.parse_args()

    import json
    out = publish(
        worktree      = args.worktree,
        remote        = args.remote,
        branch        = args.branch,
        message       = args.message,
        body          = args.body,
        history_limit = args.history_limit,
        dry_run       = args.dry_run,
    )
    print(json.dumps(out, indent=2))
    # GitHub Actions integration: expose the "changed" flag so downstream
    # summary steps can render it without grepping the JSON above.
    gh_out = os.environ.get("GITHUB_OUTPUT")
    if gh_out:
        with open(gh_out, "a", encoding="utf-8") as f:
            f.write(f"changed={'true' if out['changed'] else 'false'}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())

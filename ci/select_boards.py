#!/usr/bin/env python3
"""Pick which per-board build workflows a push to main should dispatch.

A board is selected when a changed file matches one of its trigger paths
(`render_workflows.render_paths_for_board`): its own test dir, its family's
crate paths, its own workflow file, and -- for the `core` boards only -- the
shared paths in ci/ci_common_paths.txt. The nightly sweep passes `--all` so
every board's README badge refreshes at least daily.

Prints a JSON array of workflow file names (sorted) for the dispatcher job in
nightly-platforms.yml. An unknown diff (first push, force-push, missing base)
selects every board rather than silently skipping coverage.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from functools import lru_cache

from render_workflows import load_common_paths, load_sot, render_paths_for_board

ZERO_SHA = "0" * 40


@lru_cache(maxsize=None)
def _compile(pattern: str) -> re.Pattern[str]:
    # GitHub `paths:` semantics: `**` spans directories, `*` stays in one.
    out = []
    i = 0
    while i < len(pattern):
        if pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] == "*":
            out.append("[^/]*")
            i += 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    return re.compile("".join(out) + r"\Z")


def path_matches(pattern: str, path: str) -> bool:
    return _compile(pattern).match(path) is not None


def select_workflows(boards, families, common_paths, changed):
    """Workflows to run for `changed` files; `None` means the diff is unknown."""
    if changed is None:
        return sorted(b["workflow"] for b in boards)
    selected = []
    for board in boards:
        patterns = render_paths_for_board(board, families, common_paths)
        if any(path_matches(p, f) for p in patterns for f in changed):
            selected.append(board["workflow"])
    return sorted(selected)


def changed_files(base: str, head: str):
    if not base or base == ZERO_SHA:
        return None
    result = subprocess.run(
        ["git", "diff", "--name-only", base, head],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        print(f"git diff {base}..{head} failed; selecting all boards:\n{result.stderr}", file=sys.stderr)
        return None
    return [line for line in result.stdout.splitlines() if line]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--all", action="store_true", help="select every board (scheduled sweep)")
    ap.add_argument("--base", default="", help="diff base commit (push event `before`)")
    ap.add_argument("--head", default="HEAD", help="diff head commit")
    args = ap.parse_args()

    sot = load_sot()
    changed = None if args.all else changed_files(args.base, args.head)
    workflows = select_workflows(sot["boards"], sot["families"], load_common_paths(), changed)
    print(json.dumps(workflows))
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Enforce the shrink-only baseline of `dylints/ban_raw_fbuild_path`.

FastLED/fbuild#1349 landed the lint with a baseline of the legacy files
that spell `.fbuild` by hand. That baseline is a ratchet: sanitizing a
file and deleting its line is the unit of progress, and adding a line is
how the ratchet silently unwinds. A comment in the file cannot stop that
-- the lint treats every non-comment line identically -- so this check
does.

Compares the working tree's allowlist against the same file on the
comparison ref (default `origin/main`) and fails on any added path.
Removals and comment edits are always fine. When the file does not exist
on the comparison ref (the PR that introduces it), there is nothing to
ratchet against and the check passes.

Usage:
    uv run --no-project python ci/check_fbuild_path_baseline.py [--base REF]
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ALLOWLIST = "dylints/ban_raw_fbuild_path/src/allowlist.txt"


def entries(text: str) -> set[str]:
    """The set of source paths an allowlist names, ignoring comments."""
    found: set[str] = set()
    for raw in text.splitlines():
        path = raw.split("#", 1)[0].strip()
        if path:
            found.add(path)
    return found


def read_at_ref(ref: str) -> str | None:
    """The allowlist's contents at `ref`, or None if absent there."""
    result = subprocess.run(
        ["git", "show", f"{ref}:{ALLOWLIST}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return None
    return result.stdout


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base",
        default="origin/main",
        help="git ref to compare against (default: origin/main)",
    )
    args = parser.parse_args()

    current_path = ROOT / ALLOWLIST
    if not current_path.is_file():
        print(f"{ALLOWLIST} is missing", file=sys.stderr)
        return 1

    baseline_text = read_at_ref(args.base)
    if baseline_text is None:
        print(
            f"{ALLOWLIST} does not exist at {args.base} — "
            "nothing to ratchet against yet."
        )
        return 0

    baseline = entries(baseline_text)
    current = entries(current_path.read_text(encoding="utf-8"))

    added = sorted(current - baseline)
    if added:
        print(
            f"{ALLOWLIST} is shrink-only, but this change adds "
            f"{len(added)} entr{'y' if len(added) == 1 else 'ies'}:",
            file=sys.stderr,
        )
        for path in added:
            print(f"  + {path}", file=sys.stderr)
        print(
            "\nA call site that needs a `.fbuild` path needs `fbuild_paths` "
            "(FBUILD_DIR_NAME, BUILD_DIR_NAME, get_project_fbuild_dir, "
            "get_project_build_root, BuildLayout), not an allowlist entry. "
            "See dylints/ban_raw_fbuild_path/README.md.",
            file=sys.stderr,
        )
        return 1

    removed = len(baseline - current)
    print(
        f"{ALLOWLIST}: {len(current)} entries, "
        f"{removed} removed since {args.base}, 0 added."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

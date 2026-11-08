#!/usr/bin/env python3
"""Enforce the shrink-only baseline of `dylints/ban_raw_fbuild_path`.

FastLED/fbuild#1349 landed the lint with a baseline of the legacy files
that spell `.fbuild` by hand. That baseline is a ratchet: sanitizing a
file and deleting its line is the unit of progress, and adding a line is
how the ratchet silently unwinds. A comment in the file cannot stop that
-- the lint treats every non-comment line identically -- so this check
does.

Compares the working tree's allowlist against the same file at the
comparison ref (default `origin/main`) and fails on any added path.
Removals and comment edits are always fine. When the file does not exist
at the comparison ref (the PR that introduces it), there is nothing to
ratchet against and the check passes.

The base ref is resolved to a commit FIRST and the comparison is made
against that commit. A ref git cannot resolve is a hard error, never a
pass: "nothing to ratchet against yet" is reserved for the one case where
the file is genuinely absent from a commit that exists. An unresolved
`--base` used to disable the ratchet silently, and an empty one (`--base
""`) named the index -- `git show :<path>` -- so a staged addition
compared equal to itself and the gate stayed green.

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


def git(*arguments: str) -> subprocess.CompletedProcess[str]:
    """Run git in the repository root, capturing text output."""
    return subprocess.run(
        ["git", *arguments],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )


def resolve(ref: str) -> str | None:
    """The commit `ref` names, or None when git cannot resolve it.

    An empty ref is never allowed: `git show :<path>` means "the index", so
    an empty base would compare the working tree against a copy of itself.
    """
    if not ref.strip():
        return None
    result = git("rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}")
    if result.returncode != 0:
        return None
    return result.stdout.strip() or None


def read_at_commit(commit: str) -> str | None:
    """The allowlist's contents at `commit`, or None if absent there.

    `commit` is already resolved, so a failing `cat-file -e` can only mean
    the path is missing from that commit -- not that the ref was bad.
    """
    object_name = f"{commit}:{ALLOWLIST}"
    if git("cat-file", "-e", object_name).returncode != 0:
        return None
    return git("show", object_name).stdout


def report(baseline: set[str], current: set[str], ref: str) -> int:
    """Print the ratchet verdict for `current` against `baseline`."""
    added = sorted(current - baseline)
    if not added:
        removed = len(baseline - current)
        print(
            f"{ALLOWLIST}: {len(current)} entries, "
            f"{removed} removed since {ref}, 0 added."
        )
        return 0
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


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base",
        default="origin/main",
        help="git ref to compare against (default: origin/main)",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)

    commit = resolve(args.base)
    if commit is None:
        print(
            f"cannot resolve base ref {args.base!r}: the shrink-only ratchet "
            "needs a real commit to compare against. Fetch it first, or pass "
            "--base explicitly.",
            file=sys.stderr,
        )
        return 1

    current_path = ROOT / ALLOWLIST
    if not current_path.is_file():
        print(f"{ALLOWLIST} is missing", file=sys.stderr)
        return 1

    baseline_text = read_at_commit(commit)
    if baseline_text is None:
        print(
            f"{ALLOWLIST} does not exist at {args.base} — "
            "nothing to ratchet against yet."
        )
        return 0

    current = entries(current_path.read_text(encoding="utf-8"))
    return report(entries(baseline_text), current, args.base)


if __name__ == "__main__":
    raise SystemExit(main())

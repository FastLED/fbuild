#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Linter: keep fbuild close to a monocrate — forbid adding new workspace crates.

fbuild is intentionally organized as a small, fixed set of crates. New
functionality should be folded in as a *module* inside an existing crate, not
introduced as a brand-new crate (per FastLED/fbuild#560 and the "monocrate"
policy in CLAUDE.md). Per-platform build orchestrators already live as modules
under `fbuild-build`; the running-process broker adoption lives as a module under
`fbuild-daemon` (`src/broker/`) for exactly this reason.

This guard parses the `[workspace] members` list in the root `Cargo.toml` and
fails if it contains any member that is not in the approved allowlist below. It
also fails if an approved member's directory no longer exists (so the allowlist
cannot rot).

If a new crate is *genuinely* unavoidable (a rare event that needs a maintainer
sign-off), add it to `APPROVED_MEMBERS` below in the same PR, with a one-line
rationale in the PR description. The diff to this file is the audit trail.

Usage:
    uv run python ci/check_workspace_crates.py            # report + exit 1 on violation
    uv run python ci/check_workspace_crates.py --json     # machine-readable output
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

# The complete, approved set of workspace members. Keep this list in sync with
# the root `Cargo.toml` `[workspace] members`. Adding an entry here is a
# deliberate, reviewable act — do NOT add one just to silence this check.
APPROVED_MEMBERS: frozenset[str] = frozenset(
    {
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
    }
)


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def parse_members(cargo_toml: str) -> list[str]:
    """Extract the `members = [...]` array from the `[workspace]` table.

    Deliberately a small regex parser (no toml dependency) so this runs with a
    bare `uv run --script` and no third-party packages.
    """
    match = re.search(r"members\s*=\s*\[(.*?)\]", cargo_toml, re.DOTALL)
    if not match:
        raise SystemExit("could not find `members = [...]` in root Cargo.toml")
    body = match.group(1)
    return re.findall(r'"([^"]+)"', body)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    args = parser.parse_args()

    root = repo_root()
    cargo_toml = (root / "Cargo.toml").read_text(encoding="utf-8")
    members = parse_members(cargo_toml)

    unexpected = [m for m in members if m not in APPROVED_MEMBERS]
    missing_dirs = [
        m for m in members if m in APPROVED_MEMBERS and not (root / m).is_dir()
    ]

    if args.json:
        print(
            json.dumps(
                {
                    "members": members,
                    "unexpected": unexpected,
                    "missing_dirs": missing_dirs,
                },
                indent=2,
            )
        )

    ok = not unexpected and not missing_dirs
    if ok:
        if not args.json:
            print(f"OK: {len(members)} approved workspace members, no new crates.")
        return 0

    if not args.json:
        for m in unexpected:
            print(
                f"::error::New workspace crate '{m}' is not allowed. fbuild stays "
                "close to a monocrate — fold new code into an existing crate as a "
                "module instead of adding a crate. See CLAUDE.md (monocrate policy) "
                "and FastLED/fbuild#560. If a new crate is truly justified, add it "
                "to APPROVED_MEMBERS in ci/check_workspace_crates.py with rationale "
                "in the PR."
            )
        for m in missing_dirs:
            print(
                f"::error::Approved member '{m}' has no directory — remove it from "
                "both Cargo.toml and APPROVED_MEMBERS in ci/check_workspace_crates.py."
            )
    return 1


if __name__ == "__main__":
    sys.exit(main())

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

The same guard covers the RUST-018 half of FastLED/fbuild#1639: the workspace
keeps its pinned complexity ratchets (``[workspace.lints.clippy]``) and *every*
member opts into them with ``[lints] workspace = true``. A member that forgets
that one line silently escapes ``cognitive_complexity``/``too_many_lines`` --
the ratchet then reports zero debt while that crate grows unchecked, and
nothing else in CI would notice.

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
        # FastLED/fbuild#1008 Phase B: fetch-primitives + parallel library /
        # toolchain crates behind the fbuild-packages facade.
        "crates/fbuild-packages-fetch",
        "crates/fbuild-library",
        "crates/fbuild-toolchain",
        "crates/fbuild-serial",
        "crates/fbuild-build",
        # FastLED/fbuild#1008: compile-parallelism split — shared build engine
        # + per-platform crates extracted from fbuild-build so they compile in
        # parallel behind the fbuild-build facade.
        "crates/fbuild-build-engine",
        "crates/fbuild-build-esp",
        "crates/fbuild-build-arm",
        "crates/fbuild-build-mcu",
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

# RUST-018 ratchet lints (FastLED/fbuild#1639). These are the workspace-level
# ceilings every member must inherit; lowering one to `allow`/`off` removes the
# ratchet silently rather than recording a decision.
RATCHET_LINTS: tuple[str, ...] = ("cognitive_complexity", "too_many_lines")

# Levels that switch a lint off entirely. `warn` is the pinned floor; stricter
# levels such as `deny` are accepted.
ESCAPING_LEVELS: frozenset[str] = frozenset({"allow", "off"})


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


def strip_comment(line: str) -> str:
    """Drop a trailing TOML comment, respecting quoted strings.

    A naive ``line.split("#")`` truncates ``cognitive_complexity = "warn"  #
    pinned`` at the quote boundary, and blanks a ``#`` inside a value.
    """
    quote = ""
    for index, char in enumerate(line):
        if quote:
            if char == quote:
                quote = ""
        elif char in "\"'":
            quote = char
        elif char == "#":
            return line[:index]
    return line


def tables(manifest: str) -> list[tuple[str, list[str]]]:
    """Split a manifest into ``(table name, assignment lines)`` pairs."""
    parsed: list[tuple[str, list[str]]] = [("", [])]
    for raw in manifest.splitlines():
        line = strip_comment(raw).strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            parsed.append((line[1:-1].strip(), []))
            continue
        parsed[-1][1].append(line)
    return parsed


def assignments(lines: list[str]) -> dict[str, str]:
    """Map ``key = value`` lines to key -> unquoted value."""
    found: dict[str, str] = {}
    for line in lines:
        if "=" not in line:
            continue
        key, _, value = line.partition("=")
        found[key.strip()] = value.strip().strip("\"'")
    return found


def parse_clippy_ratchets(cargo_toml: str) -> dict[str, str]:
    """The levels declared in ``[workspace.lints.clippy]``."""
    for name, lines in tables(cargo_toml):
        if name == "workspace.lints.clippy":
            return assignments(lines)
    return {}


def inherits_workspace_lints(manifest: str) -> bool:
    """Whether a member opts into the workspace lint table.

    Accepts both spellings cargo allows: the ``[lints]`` table form and the
    dotted ``lints.workspace = true`` top-level form.
    """
    for name, lines in tables(manifest):
        if name == "lints" and assignments(lines).get("workspace") == "true":
            return True
        if name == "" and assignments(lines).get("lints.workspace") == "true":
            return True
    return False


def lint_inheritance_errors(root: Path) -> list[str]:
    """Every RUST-018 invariant a workspace member must satisfy."""
    cargo_toml = (root / "Cargo.toml").read_text(encoding="utf-8")
    errors: list[str] = []

    ratchets = parse_clippy_ratchets(cargo_toml)
    for lint in RATCHET_LINTS:
        level = ratchets.get(lint)
        if level is None:
            errors.append(
                f"::error::`[workspace.lints.clippy]` no longer declares `{lint}`. "
                "That removes the RUST-018 ratchet without a decision: keep it at "
                '"warn" or stricter, and record new debt as a per-function '
                "`#[expect(..., reason = ...)]`."
            )
        elif level in ESCAPING_LEVELS:
            errors.append(
                f"::error::`[workspace.lints.clippy]` sets `{lint} = \"{level}\"`, "
                "which switches the RUST-018 ratchet off. Keep the pinned "
                '"warn" floor and suppress a single offender with '
                f"`#[expect(clippy::{lint}, reason = ...)]` instead."
            )

    for member in parse_members(cargo_toml):
        manifest = root / member / "Cargo.toml"
        if not manifest.is_file():
            continue  # reported as a missing directory by the caller
        if not inherits_workspace_lints(manifest.read_text(encoding="utf-8")):
            errors.append(
                f"::error::Workspace member '{member}' does not inherit the "
                f"workspace lints. Add\n    [lints]\n    workspace = true\n"
                f"to {member}/Cargo.toml -- without it `{RATCHET_LINTS[0]}` and "
                f"`{RATCHET_LINTS[1]}` are not checked in that crate at all "
                "(FastLED/fbuild#1639, RUST-018)."
            )
    return errors


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
    inheritance = lint_inheritance_errors(root)

    if args.json:
        print(
            json.dumps(
                {
                    "members": members,
                    "unexpected": unexpected,
                    "missing_dirs": missing_dirs,
                    "lint_inheritance": inheritance,
                },
                indent=2,
            )
        )

    ok = not unexpected and not missing_dirs and not inheritance
    if ok:
        if not args.json:
            print(
                f"OK: {len(members)} approved workspace members, no new crates,"
                " all inherit the workspace lints."
            )
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
        for message in inheritance:
            print(message)
    return 1


if __name__ == "__main__":
    sys.exit(main())

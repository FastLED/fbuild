#!/usr/bin/env python3
"""Fail when a Dylint allowlist cannot excuse the file it names.

Every lint in ``dylints/*/src`` loads its exemptions with
``include_str!("allowlist.txt")`` and matches them the same way
(``lines() -> trim -> skip blank/``#``-prefixed -> source_path.ends_with(entry)``
where ``source_path`` has ``\\`` rewritten to ``/``). This script checks the
allowlists against *that* contract, because an entry which cannot match is an
exemption that silently does not exist:

* the named path is not a file in this repository,
* the line carries an inline ``#`` comment (every lint matches the WHOLE
  trimmed line, so ``crates/x.rs  # why`` never matches -- the reason must be
  on its own comment line),
* the entry holds a backslash (source paths are `/`-normalized first),
* the entry is absolute or contains ``..`` (no source path under the repo
  root can end with it),
* the allowlist file is not read by its lint at all (orphan: no exemption it
  lists has any effect).
"""

from pathlib import Path, PurePosixPath
import sys


ROOT = Path(__file__).resolve().parents[1]

# Every lint reads its allowlist with exactly this expression.
INCLUDE_EXPR = 'include_str!("allowlist.txt")'

MISSING = "missing file"
DEAD = "entry can never match"
ORPHAN = "orphan allowlist"


def allowlist_files(root: Path) -> list[Path]:
    return sorted((root / "dylints").glob("*/src/allowlist.txt"))


def entry_of(raw: str) -> str:
    """The entry a lint would see for this line, or "" when it is not an entry."""
    line = raw.strip()
    if not line or line.startswith("#"):
        return ""
    return line


def entry_error(entry: str) -> str | None:
    """Why no source path can end with this entry, or None when it is plausible."""
    if "#" in entry:
        return (
            "inline comment -- the lints match the whole trimmed line, so this "
            "entry never applies; put the reason on its own '#' line"
        )
    if "\\" in entry:
        return "backslash -- source paths are '/' normalized before matching"
    pure = PurePosixPath(entry)
    if pure.is_absolute():
        return "absolute path -- no repository source path ends with it"
    if ".." in pure.parts:
        return "escapes the repository root -- no source path ends with it"
    return None


def lint_reads(allowlist: Path) -> bool:
    lib = allowlist.parent / "lib.rs"
    if not lib.is_file():
        return False
    return INCLUDE_EXPR in lib.read_text(encoding="utf-8")


def check(root: Path) -> list[str]:
    """Every reason this root's Dylint allowlists are not current."""
    allowlists = allowlist_files(root)
    if not allowlists:
        return [f"no Dylint allowlists found under {root / 'dylints'}"]
    problems: list[str] = []
    for allowlist in allowlists:
        name = allowlist.relative_to(root)
        lines = allowlist.read_text(encoding="utf-8").splitlines()
        if not lint_reads(allowlist) and any(entry_of(raw) for raw in lines):
            problems.append(
                f"{name} -> {ORPHAN}: src/lib.rs does not {INCLUDE_EXPR}, "
                "so nothing in this file excuses anything"
            )
        for number, raw in enumerate(lines, 1):
            entry = entry_of(raw)
            if not entry:
                continue
            reason = entry_error(entry)
            if reason is not None:
                problems.append(f"{name}:{number} -> {DEAD}: {entry} ({reason})")
            elif not (root / entry).is_file():
                problems.append(f"{name}:{number} -> {MISSING}: {entry}")
    return problems


def main() -> int:
    problems = check(ROOT)
    if problems:
        print("stale Dylint allowlists:", file=sys.stderr)
        print("\n".join(problems), file=sys.stderr)
        return 1
    print("Dylint allowlist paths are current.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

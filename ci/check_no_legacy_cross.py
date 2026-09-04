#!/usr/bin/env python3
"""Forbid the retired cross-compilation backends: zig, cargo-zigbuild, xwin.

soldr's blessed cross path (`soldr prepare --target X` + `soldr build --target
X`) selects and materializes the compiler, linker, SDK and sysroot for every
supported triple. `soldr prepare --help` states the contract directly: "Legacy
backend wrappers are diagnostic-only overrides and are never selected by this
command."

Before that existed, this repo reached for the wrappers directly, and each one
cost us:

  * `cargo-zigbuild` was pip-installed UNPINNED. It floated 0.23.1 -> 0.23.4
    mid-release and broke both apple-darwin lanes of 2.5.22 with
    `unable to read exported symbols list '-dead_strip': FileNotFound`,
    because 0.23.4 reorders the linker args rustc emits for a cdylib.
  * `cargo-xwin` needed ~40 lines of CRT-casing symlink repair in the
    workflow to make a case-sensitive filesystem match MSVC's import
    libraries.
  * soldr's bin cache served a corrupted `cargo-zigbuild` binary
    (`Syntax error: ")" unexpected`), which forced a `--no-cache` bypass.

None of that is our problem to carry any more. This gate keeps it from coming
back by accident -- a copy-pasted workflow snippet or an agent reaching for
the recipe it remembers.

Run with no arguments:

    uv run --no-project python ci/check_no_legacy_cross.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Directories worth scanning. Everything else is either generated, vendored,
# or not a place a build recipe can hide.
SCAN_DIRS = (".github", "ci", "crates", "docs", "agents", "dylints")
SCAN_ROOT_FILES = ("CLAUDE.md", "README.md", "Cargo.toml", "pyproject.toml")

SCAN_SUFFIXES = {".yml", ".yaml", ".sh", ".py", ".rs", ".toml", ".md", ".bash"}
SCAN_NAMES = {"Dockerfile"}

SKIP_DIR_PARTS = {".git", "target", "node_modules", "__pycache__", ".venv"}

# Each pattern is (regex, human-readable rule).
PATTERNS: list[tuple[re.Pattern[str], str]] = [
    (
        re.compile(r"\bcargo[- ]zigbuild\b"),
        "cargo-zigbuild -- use `soldr build --target <triple>`",
    ),
    (
        re.compile(r"\bziglang\b"),
        "ziglang -- soldr materializes its own toolchain",
    ),
    (
        # `\b` cannot terminate `c++` -- `+` is not a word character, so a
        # trailing `\b` would need a word char after it and `zig c++ -o` would
        # slip through. Assert "not continuing an identifier" instead.
        re.compile(r"\bzig\s+(?:cc|c\+\+|build-exe)(?![\w-])"),
        "zig as a C compiler -- use `soldr cc` / `soldr c++`",
    ),
    (
        re.compile(r"\bcargo[- ]xwin\b"),
        "cargo-xwin -- `soldr build --target *-pc-windows-msvc` owns the xwin cache",
    ),
]

# Files that are ALLOWED to name the retired backends, and why. A path here
# still may not contain an *invocation*; see INVOCATION below.
ALLOWLIST: dict[str, str] = {
    "ci/check_no_legacy_cross.py": "this gate names what it forbids",
    "ci/test_no_legacy_cross.py": "unit tests for this gate",
    "agents/docs/cross-compilation.md": (
        "the doc that explains why these are banned and shows the measured "
        "glibc floors; it must be able to name them"
    ),
    "ci/bench-results/REPORT.md": "frozen benchmark record of a past run",
}

# A mention inside a comment is history; a mention in a runnable line is a
# relapse. Comment prefixes per file type we scan.
COMMENT_PREFIXES = ("#", "//", "--", "*", "<!--")


# An inline escape hatch for the rare line that legitimately needs a retired
# backend. Precise by construction: it exempts ONE line, names the reason at
# the point of use, and shows up in every diff that touches it -- unlike a
# whole-file allowlist entry, which silently covers future additions too.
PRAGMA = "lint-allow: legacy-cross"


def is_exempt(lines: list[str], index: int) -> bool:
    """True when the line, or the line above it, carries the pragma."""
    if PRAGMA in lines[index]:
        return True
    return index > 0 and PRAGMA in lines[index - 1]


def is_comment(line: str) -> bool:
    stripped = line.strip()
    return any(stripped.startswith(prefix) for prefix in COMMENT_PREFIXES)


def iter_files():
    for name in SCAN_ROOT_FILES:
        path = ROOT / name
        if path.is_file():
            yield path
    for directory in SCAN_DIRS:
        base = ROOT / directory
        if not base.is_dir():
            continue
        for path in base.rglob("*"):
            if not path.is_file():
                continue
            if SKIP_DIR_PARTS & set(path.parts):
                continue
            if path.suffix in SCAN_SUFFIXES or path.name in SCAN_NAMES:
                yield path


def scan() -> list[str]:
    findings: list[str] = []
    for path in sorted(iter_files()):
        rel = path.relative_to(ROOT).as_posix()
        if rel in ALLOWLIST:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        lines = text.splitlines()
        for lineno, line in enumerate(lines, start=1):
            if is_exempt(lines, lineno - 1):
                continue
            if is_comment(line):
                # Comments may record the history -- that is how the next
                # reader learns why these are gone.
                continue
            for pattern, rule in PATTERNS:
                if pattern.search(line):
                    findings.append(f"{rel}:{lineno}: {rule}\n    {line.strip()}")
    return findings


def main() -> int:
    findings = scan()
    if findings:
        print("Retired cross-compilation backend found in a runnable line:\n")
        for finding in findings:
            print(f"  {finding}")
        print(
            "\nsoldr's blessed path replaces all three:\n"
            "    soldr prepare --target <triple>\n"
            "    soldr build --release --target <triple> -p <crate>\n"
            "\nIf ONE line genuinely needs a retired backend, put\n"
            f"    {PRAGMA}: <reason>\n"
            "in a comment directly above it. Whole files go in ALLOWLIST\n"
            "in ci/check_no_legacy_cross.py, with a reason a reviewer accepts."
        )
        return 1
    print("OK: no cargo-zigbuild / ziglang / zig-cc / cargo-xwin invocations")
    return 0


if __name__ == "__main__":
    sys.exit(main())

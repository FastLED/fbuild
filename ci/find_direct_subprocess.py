#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Linter: forbid direct `std::process::Command::new` / `tokio::process::Command::new`
outside of documented hold-outs.

Every subprocess fbuild starts must go through the
`fbuild-core::subprocess` wrappers (which are themselves backed by
[`running-process`](https://github.com/zackees/running-process) so
that containment, concurrent pipe draining, and Windows-specific env
handling are implemented once and cannot drift.

Tracked by FastLED/fbuild#141.

A site is allowlisted by placing this marker on the same line or on
the line immediately before the `Command::new(`:

    // allow-direct-spawn: <one-line reason>

The reason is mandatory: a bare `// allow-direct-spawn` with nothing after
the colon does NOT allowlist a site, because an unexplained hold-out is
indistinguishable from a forgotten migration. Occurrences inside comments
(`//`, `/* ... */`, nested) and inside string/raw-string literals are
ignored: those are documentation, not spawns.

Intentional hold-outs currently allowed:
- Daemon spawns from CLI/Python/tests (daemon must outlive parent).
- zccache daemon bootstrap (independent lifecycle).
- containment module's own regression tests.
- Integration test harnesses that spawn binaries under test.
- tokio async streaming emulator handlers (QEMU, avr8js/node) where
  NativeProcess's blocking API is unsuitable.
- tokio parallel async fan-out in the CLI (IWYU, clang-tidy) inside a
  process that has no daemon containment group.

Run in CI with `--fail` so any new direct spawn without a marker
breaks the build.

Usage:
    uv run python ci/find_direct_subprocess.py            # report
    uv run python ci/find_direct_subprocess.py --fail     # exit 1 if any
    uv run python ci/find_direct_subprocess.py --json     # machine output
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
CRATES_DIR = REPO_ROOT / "crates"

# Matches:  std::process::Command::new(   |   tokio::process::Command::new(   |   Command::new(
COMMAND_NEW = re.compile(r"\b(?:(?:std|tokio)::process::)?Command::new\s*\(")

ALLOW_MARKER = "allow-direct-spawn"


@dataclass(frozen=True)
class Hit:
    path: Path
    line_no: int
    text: str
    allowlisted: bool
    reason: str | None


_IDENT_CHARS = frozenset("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_")


def _blank(chars: list[str], start: int, end: int) -> None:
    for i in range(start, end):
        chars[i] = " "


def _starts_raw_string(line: str, i: int) -> bool:
    """True when `line[i]` opens a Rust raw string literal (`r"..."`, `r#"..."#`)."""
    if i > 0 and line[i - 1] in _IDENT_CHARS:
        return False
    j = i + 1
    while j < len(line) and line[j] == "#":
        j += 1
    return j < len(line) and line[j] == '"'


def _string_end(line: str, start: int) -> int:
    """Index just past the escaped string literal opening at `start`."""
    i = start + 1
    while i < len(line):
        if line[i] == "\\":
            i += 2
            continue
        if line[i] == '"':
            return i + 1
        i += 1
    return len(line)


def _raw_string_end(line: str, start: int) -> int:
    """Index just past the raw string literal opening at `start`."""
    i = start + 1
    hashes = 0
    while i < len(line) and line[i] == "#":
        hashes += 1
        i += 1
    terminator = '"' + "#" * hashes
    end = line.find(terminator, i + 1)
    return len(line) if end < 0 else end + len(terminator)


def _char_end(line: str, start: int) -> int | None:
    """Index just past the char literal opening at `start`, or None for a lifetime.

    A char literal is a quote, at most a short escape, and a closing quote;
    `'a` in a lifetime has no closing quote nearby. Without this the quote in
    `'"'` would open a phantom string literal and hide a real spawn after it.
    """
    i = start + 1
    limit = min(len(line), start + 12)
    while i < limit:
        if line[i] == "\\":
            i += 2
            continue
        if line[i] == "'":
            return i + 1
        i += 1
    return None


def _skip_block(line: str, i: int, depth: int) -> tuple[int, int]:
    """Consume block-comment characters from `i`, honouring Rust's nesting.

    Returns the index to resume scanning at and the new open-comment depth.
    """
    while i < len(line):
        close = line.find("*/", i)
        nested = line.find("/*", i)
        if nested != -1 and (close == -1 or nested < close):
            depth += 1
            i = nested + 2
            continue
        if close == -1:
            return len(line), depth
        depth -= 1
        i = close + 2
        if depth == 0:
            return i, 0
    return i, depth


def code_only(line: str, depth: int) -> tuple[str, int]:
    """Blank out comments and string literals, keeping columns stable.

    `depth` carries the open `/* ... */` nesting count across lines (Rust
    block comments nest). Returns the code-only spelling of the line and the
    new depth, so a `Command::new(` mentioned in prose or in a test fixture
    string is never mistaken for a spawn.
    """
    chars = list(line)
    width = len(line)
    i = 0
    while i < width:
        if depth > 0:
            resume, depth = _skip_block(line, i, depth)
            _blank(chars, i, resume)
            i = resume
            continue
        if line.startswith("//", i):
            _blank(chars, i, width)
            return "".join(chars), 0
        if line.startswith("/*", i):
            depth += 1
            _blank(chars, i, i + 2)
            i += 2
            continue
        if line[i] == '"':
            end = _string_end(line, i)
            _blank(chars, i, end)
            i = end
            continue
        if line[i] == "r" and _starts_raw_string(line, i):
            end = _raw_string_end(line, i)
            _blank(chars, i, end)
            i = end
            continue
        if line[i] == "'" and (end := _char_end(line, i)) is not None:
            _blank(chars, i, end)
            i = end
            continue
        i += 1
    return "".join(chars), depth


def _allowlist_reason(lines: list[str], idx: int) -> str | None:
    """Return the allowlist reason if this hit is annotated, else None.

    The marker may appear on the same line as the hit (trailing comment)
    or on the line immediately above it.
    """
    candidates = [lines[idx]]
    if idx > 0:
        candidates.append(lines[idx - 1])
    for candidate in candidates:
        if ALLOW_MARKER not in candidate:
            continue
        reason = candidate.split(ALLOW_MARKER, 1)[1].lstrip(": ").strip()
        if reason:
            return reason
    return None


def scan_file(path: Path) -> list[Hit]:
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()
    hits: list[Hit] = []
    depth = 0
    for idx, line in enumerate(lines):
        code, depth = code_only(line, depth)
        if not COMMAND_NEW.search(code):
            continue
        reason = _allowlist_reason(lines, idx)
        hits.append(
            Hit(
                path=path,
                line_no=idx + 1,
                text=line.rstrip(),
                allowlisted=reason is not None,
                reason=reason,
            )
        )
    return hits


def scan_workspace() -> list[Hit]:
    if not CRATES_DIR.is_dir():
        sys.stderr.write(f"error: crates dir not found at {CRATES_DIR}\n")
        sys.exit(2)
    out: list[Hit] = []
    for rs in sorted(CRATES_DIR.rglob("*.rs")):
        # Skip target/ output if it ever lands inside crates/.
        if "target" in rs.parts:
            continue
        out.extend(scan_file(rs))
    return out


def render_text(hits: list[Hit]) -> str:
    lines: list[str] = []
    pending = [h for h in hits if not h.allowlisted]
    allowed = [h for h in hits if h.allowlisted]
    lines.append(f"Direct Command::new sites: {len(hits)}")
    lines.append(f"  to migrate: {len(pending)}")
    lines.append(f"  allowlisted: {len(allowed)}")
    if pending:
        lines.append("")
        lines.append(
            "NEW direct spawns without an `allow-direct-spawn: <reason>` marker:"
        )
        lines.append(
            "  (route via fbuild_core::subprocess::{run_command,run_command_passthrough}"
        )
        lines.append("   or annotate with a one-line reason — see FastLED/fbuild#141)")
        for h in pending:
            rel = h.path.relative_to(REPO_ROOT)
            lines.append(f"  {rel}:{h.line_no}: {h.text.strip()}")
        if any(ALLOW_MARKER in h.text for h in pending):
            lines.append(
                "  note: a `allow-direct-spawn` marker with no reason after the colon"
                " does not allowlist a site"
            )
    if allowed:
        lines.append("")
        lines.append("Allowlisted (intentional hold-outs):")
        for h in allowed:
            rel = h.path.relative_to(REPO_ROOT)
            lines.append(f"  {rel}:{h.line_no}: {h.reason}")
    return "\n".join(lines)


def render_json(hits: list[Hit]) -> str:
    payload = {
        "total": len(hits),
        "to_migrate": sum(1 for h in hits if not h.allowlisted),
        "allowlisted": sum(1 for h in hits if h.allowlisted),
        "hits": [
            {
                "path": str(h.path.relative_to(REPO_ROOT)),
                "line": h.line_no,
                "text": h.text.strip(),
                "allowlisted": h.allowlisted,
                "reason": h.reason,
            }
            for h in hits
        ],
    }
    return json.dumps(payload, indent=2)


def unmigrated(hits: list[Hit]) -> list[Hit]:
    """Sites counted as violations: unannotated, or annotated without a reason."""
    return [hit for hit in hits if not hit.allowlisted]


def exit_code(hits: list[Hit], fail: bool) -> int:
    return 1 if fail and unmigrated(hits) else 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--fail", action="store_true", help="exit 1 when >0 unmigrated sites")
    p.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    args = p.parse_args()

    hits = scan_workspace()
    print(render_json(hits) if args.json else render_text(hits))

    return exit_code(hits, args.fail)


if __name__ == "__main__":
    sys.exit(main())

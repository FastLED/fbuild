#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Find direct std::process / tokio::process spawn sites in fbuild crates.

Goal: every subprocess that fbuild starts should go through
`running-process` (via the wrappers in `fbuild-core::subprocess` and
`fbuild-core::containment`). This script enumerates every remaining
direct `Command::new(...)` site so they can be migrated and eliminated
one PR at a time.

A site is allowlisted by placing this marker on the same line or on
the line immediately before the `Command::new(`:

    // allow-direct-spawn: <one-line reason>

When this script reports zero non-allowlisted sites across the whole
workspace, delete it (and the marker comments) and rely on
`running-process` exclusively. Tracked by FastLED/fbuild#<issue>.

Usage:
    uv run python ci/find_direct_subprocess.py            # report
    uv run python ci/find_direct_subprocess.py --fail     # exit 1 if >0
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


def _is_doc_or_string(line: str) -> bool:
    """Skip occurrences inside doc comments or string literals.

    We only care about live code, not references in docstrings or
    rustdoc comments that name `Command::new()` for explanation.
    """
    stripped = line.lstrip()
    if stripped.startswith("//") or stripped.startswith("///"):
        return True
    if stripped.startswith("//!") or stripped.startswith("*"):
        return True
    return False


def _allowlist_reason(lines: list[str], idx: int) -> str | None:
    """Return the allowlist reason if this hit is annotated, else None.

    The marker may appear on the same line as the hit (trailing comment)
    or on the line immediately above it.
    """
    same = lines[idx]
    above = lines[idx - 1] if idx > 0 else ""
    for candidate in (same, above):
        if ALLOW_MARKER in candidate:
            tail = candidate.split(ALLOW_MARKER, 1)[1]
            return tail.lstrip(": ").strip() or "<no reason given>"
    return None


def scan_file(path: Path) -> list[Hit]:
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()
    hits: list[Hit] = []
    for idx, line in enumerate(lines):
        if not COMMAND_NEW.search(line):
            continue
        if _is_doc_or_string(line):
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
        lines.append("To migrate (no allow-direct-spawn marker):")
        for h in pending:
            rel = h.path.relative_to(REPO_ROOT)
            lines.append(f"  {rel}:{h.line_no}: {h.text.strip()}")
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


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--fail", action="store_true", help="exit 1 when >0 unmigrated sites")
    p.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    args = p.parse_args()

    hits = scan_workspace()
    print(render_json(hits) if args.json else render_text(hits))

    if args.fail and any(not h.allowlisted for h in hits):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

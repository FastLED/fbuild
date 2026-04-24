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

# Directories we never want to scan, relative to REPO_ROOT.
EXCLUDED_DIR_PARTS = {"target", ".git", "node_modules", ".venv", "venv"}

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


def _is_doc_or_comment(line: str) -> bool:
    """Skip occurrences inside pure doc/line comments.

    Only checks whether the *line* is effectively a comment (starts
    with `//`, `///`, `//!`, or a `*` continuation inside a `/* */`
    block).  String-literal filtering is handled separately by
    :func:`_strip_rust_literals` because a literal can appear on a
    line that is otherwise real code.
    """
    stripped = line.lstrip()
    if stripped.startswith("//") or stripped.startswith("///"):
        return True
    if stripped.startswith("//!") or stripped.startswith("*"):
        return True
    return False


def _strip_rust_literals(src: str) -> str:
    """Return ``src`` with every Rust string / char literal replaced by
    same-length spaces.

    This is a small state machine that understands:

    * Normal strings     — ``"..."`` with escapes (``\\"``, ``\\\\``, etc.)
    * Char literals      — ``'x'``, ``'\\n'`` (but NOT lifetimes: ``'a``)
    * Raw strings        — ``r"..."``, ``r#"..."#``, ``r##"..."##`` (any ``#``s)
    * Byte strings       — ``b"..."``, ``br"..."``, ``br#"..."#``
    * Line comments      — ``// ...``   (to end of line)
    * Block comments     — ``/* ... */`` (may nest)

    It is intentionally whole-file aware: callers should pass the
    complete source text, not a single line, because block comments
    and raw strings can span lines.

    Regex matches outside of any of these regions are preserved; any
    ``Command::new(`` that lives inside a string therefore becomes
    blanks and will not match the scanner regex.
    """
    n = len(src)
    out = list(src)
    i = 0
    # Block comments can nest per the Rust reference.
    block_depth = 0

    def blank(start: int, end: int) -> None:
        for k in range(start, min(end, n)):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        ch = src[i]
        nxt = src[i + 1] if i + 1 < n else ""

        # --- inside a block comment -------------------------------------------------
        if block_depth > 0:
            if ch == "/" and nxt == "*":
                block_depth += 1
                blank(i, i + 2)
                i += 2
                continue
            if ch == "*" and nxt == "/":
                block_depth -= 1
                blank(i, i + 2)
                i += 2
                continue
            # Preserve newlines so that line numbers stay aligned.
            if ch != "\n":
                out[i] = " "
            i += 1
            continue

        # --- start of a block / line comment ---------------------------------------
        if ch == "/" and nxt == "*":
            block_depth = 1
            blank(i, i + 2)
            i += 2
            continue
        if ch == "/" and nxt == "/":
            # Blank to end of line.
            j = src.find("\n", i)
            if j == -1:
                j = n
            blank(i, j)
            i = j
            continue

        # --- raw / byte strings (b, r, br, with any number of #s) -----------------
        if ch in ("r", "b"):
            # Determine prefix: r, b, br, rb(?) — rb is NOT valid in Rust, but
            # handle b"..." and br"..." / r"...".
            j = i
            has_r = False
            has_b = False
            if src[j] == "b":
                has_b = True
                j += 1
            if j < n and src[j] == "r":
                has_r = True
                j += 1
            # At this point j points just past the prefix letters.
            if has_r and j < n:
                # Collect any number of '#'
                hash_count = 0
                while j < n and src[j] == "#":
                    hash_count += 1
                    j += 1
                if j < n and src[j] == '"':
                    # Raw string: find closing `"` followed by hash_count '#'s.
                    start = i
                    j += 1  # past opening "
                    close_token = '"' + ("#" * hash_count)
                    end = src.find(close_token, j)
                    if end == -1:
                        end = n
                    else:
                        end += len(close_token)
                    blank(start, end)
                    i = end
                    continue
            elif has_b and j < n and src[j] == '"':
                # Byte string: behaves like a normal string w.r.t. escapes.
                start = i
                j += 1  # past opening "
                while j < n:
                    c = src[j]
                    if c == "\\" and j + 1 < n:
                        j += 2
                        continue
                    if c == '"':
                        j += 1
                        break
                    j += 1
                blank(start, j)
                i = j
                continue
            # Not a string after all: fall through and treat the identifier
            # character normally.

        # --- normal string literal -------------------------------------------------
        if ch == '"':
            start = i
            j = i + 1
            while j < n:
                c = src[j]
                if c == "\\" and j + 1 < n:
                    j += 2
                    continue
                if c == '"':
                    j += 1
                    break
                j += 1
            blank(start, j)
            i = j
            continue

        # --- char literal vs lifetime ----------------------------------------------
        if ch == "'":
            # Heuristic: if it looks like a lifetime (`'ident`, not
            # immediately followed by an escape or a character+`'`), leave it
            # alone. Otherwise blank until the closing `'`.
            j = i + 1
            if j < n and src[j] == "\\":
                # Escaped char literal: '\n', '\\', '\'', '\xNN', '\u{..}'.
                end = src.find("'", j + 1)
                if end != -1:
                    blank(i, end + 1)
                    i = end + 1
                    continue
            elif j + 1 < n and src[j + 1] == "'":
                # Plain single-char literal: 'x'
                blank(i, j + 2)
                i = j + 2
                continue
            # Otherwise: treat as a lifetime or stray quote — skip one char.
            i += 1
            continue

        i += 1

    return "".join(out)


# Backwards-compat shim (retained in case external callers imported it).
def _is_doc_or_string(line: str) -> bool:
    return _is_doc_or_comment(line)


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
    raw = path.read_text(encoding="utf-8", errors="replace")
    raw_lines = raw.splitlines()
    # Strip literals / comments file-wide, then split into aligned lines
    # so matches inside strings vanish but line numbers stay intact.
    stripped = _strip_rust_literals(raw)
    stripped_lines = stripped.splitlines()
    hits: list[Hit] = []
    for idx, sline in enumerate(stripped_lines):
        if not COMMAND_NEW.search(sline):
            continue
        if _is_doc_or_comment(sline):
            continue
        raw_line = raw_lines[idx] if idx < len(raw_lines) else sline
        reason = _allowlist_reason(raw_lines, idx)
        hits.append(
            Hit(
                path=path,
                line_no=idx + 1,
                text=raw_line.rstrip(),
                allowlisted=reason is not None,
                reason=reason,
            )
        )
    return hits


def _iter_rust_sources(root: Path):
    """Yield every ``*.rs`` file under ``root``, skipping excluded dirs.

    Covers ``crates/**``, ``examples/**``, ``benches/**``, ``tests/**``,
    top-level ``build.rs``, any other ``**/*.rs``.
    """
    for rs in root.rglob("*.rs"):
        try:
            rel_parts = rs.relative_to(root).parts
        except ValueError:
            rel_parts = rs.parts
        if any(part in EXCLUDED_DIR_PARTS for part in rel_parts):
            continue
        yield rs


def scan_workspace() -> list[Hit]:
    if not REPO_ROOT.is_dir():
        sys.stderr.write(f"error: repo root not found at {REPO_ROOT}\n")
        sys.exit(2)
    out: list[Hit] = []
    for rs in sorted(_iter_rust_sources(REPO_ROOT)):
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


# ---------------------------------------------------------------------------
# Self-tests for the literal-aware scanner.  Run via:
#
#     uv run python ci/find_direct_subprocess.py --self-test
#
# These exist inline (rather than under tests/) so the linter stays a
# single self-contained script.
# ---------------------------------------------------------------------------


def _self_test() -> int:
    import tempfile

    failures: list[str] = []

    def check(name: str, cond: bool, detail: str = "") -> None:
        if not cond:
            failures.append(f"FAIL {name}: {detail}")

    # 1. string literal containing `Command::new(` — MUST NOT be flagged.
    src_string = r'''
fn demo() {
    let s = "Command::new(";
    let _ = s;
}
'''
    stripped = _strip_rust_literals(src_string)
    check(
        "string literal elided",
        not COMMAND_NEW.search(stripped),
        f"stripped={stripped!r}",
    )

    # 2. raw string with `#` — MUST NOT be flagged.
    src_raw = r'''
fn demo() {
    let s = r#"Command::new( in raw "#;
    let _ = s;
}
'''
    stripped = _strip_rust_literals(src_raw)
    check(
        "raw string elided",
        not COMMAND_NEW.search(stripped),
        f"stripped={stripped!r}",
    )

    # 2b. byte string — MUST NOT be flagged.
    src_byte = r'''
fn demo() {
    let s = b"Command::new(";
    let _ = s;
}
'''
    stripped = _strip_rust_literals(src_byte)
    check(
        "byte string elided",
        not COMMAND_NEW.search(stripped),
        f"stripped={stripped!r}",
    )

    # 3. real spawn — MUST be flagged.
    src_real = r'''
use std::process::Command;
fn demo() {
    let _ = Command::new("echo").spawn();
}
'''
    stripped = _strip_rust_literals(src_real)
    check(
        "real spawn preserved",
        bool(COMMAND_NEW.search(stripped)),
        f"stripped={stripped!r}",
    )

    # 4. block comment containing the pattern — MUST NOT be flagged.
    src_block = r'''
fn demo() {
    /* Command::new(foo) */
    let _ = 1;
}
'''
    stripped = _strip_rust_literals(src_block)
    check(
        "block comment elided",
        not COMMAND_NEW.search(stripped),
        f"stripped={stripped!r}",
    )

    # 5. end-to-end: build.rs in a temp dir gets scanned and a real hit is found.
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        (root / "build.rs").write_text(
            "fn main() { std::process::Command::new(\"cc\"); }\n",
            encoding="utf-8",
        )
        # Pretend this temp dir is the repo root for the duration of the test.
        sources = list(_iter_rust_sources(root))
        check(
            "top-level build.rs discovered",
            any(p.name == "build.rs" for p in sources),
            f"sources={sources}",
        )
        hits = []
        for rs in sources:
            hits.extend(scan_file(rs))
        check(
            "build.rs hit detected",
            any(not h.allowlisted for h in hits) and len(hits) == 1,
            f"hits={hits}",
        )

    # 6. string false positive is NOT flagged by scan_file.
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        f = root / "a.rs"
        f.write_text(
            'fn demo() { let s = "Command::new("; let _ = s; }\n',
            encoding="utf-8",
        )
        hits = scan_file(f)
        check(
            "string literal produces no hit",
            hits == [],
            f"hits={hits}",
        )

    if failures:
        for line in failures:
            print(line)
        print(f"\n{len(failures)} self-test failure(s)")
        return 1
    print("self-tests: OK")
    return 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--fail", action="store_true", help="exit 1 when >0 unmigrated sites")
    p.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    p.add_argument(
        "--self-test",
        action="store_true",
        help="run built-in scanner tests and exit",
    )
    args = p.parse_args()

    if args.self_test:
        return _self_test()

    hits = scan_workspace()
    print(render_json(hits) if args.json else render_text(hits))

    if args.fail and any(not h.allowlisted for h in hits):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

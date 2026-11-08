#!/usr/bin/env python3
"""Audit `#[ignore]` attribute usage across the fbuild Rust workspace.

This script walks every `*.rs` file under the workspace (skipping `target/`
and `.git/`), finds every `#[ignore]` and `#[ignore = "..."]` attribute, and
emits an inventory in CSV (default) or markdown.

Design notes:
- Only stdlib (re, csv, argparse, pathlib). No third-party dependencies — the
  workflow invokes this via `uv run python ci/audit_ignored_tests.py`.
- Detection runs on a *blanked* copy of the source (`code_only`): comments and
  string/char literals are replaced by spaces, columns and newlines preserved.
  A line-start regex alone is not enough — a fixture inside `r#"..."#` or a
  `'\"'` char literal both move the text that a naive matcher sees, which either
  invents an ignored test or hides a real one.
- The reason string is read back from the RAW line (the blanked copy has it
  erased); escapes are kept verbatim because this is an inventory, not codegen.
- The "test_name" is the first `fn <name>` (or `pub fn <name>`) encountered
  after the attribute, skipping intervening attributes/comments/blank lines.
- Bare `#[ignore]` records an empty reason — these are policy violations under
  #839, so the markdown report surfaces the count.
- First-seen dates are intentionally NOT computed in this pass. That's a
  follow-up — the goal here is a current-state inventory the cron can diff.

Outputs:
- `--csv` (default): `file,test_name,reason,line` with a header row.
- `--markdown`: a GitHub-flavored markdown table suitable for the body of the
  tracking issue, with a count summary.

Exit codes:
- 0 on success regardless of inventory size — the script reports, it does
  not gate.

See FastLED/fbuild#839 for the policy this script enforces visibility for.
"""

from __future__ import annotations

import argparse
import csv
import io
import re
import sys
from pathlib import Path
from typing import Iterator

# A complete `#[ignore]`, `#[ignore = "reason"]` or `#[ignore = r#"reason"#]`
# attribute is parsed structurally by `parse_ignore_attribute`; these regexes
# only locate the pieces.  Both run against the blanked (code-only) line, so a
# comment or a string literal can never look like an attribute.
ATTRIBUTE_RE = re.compile(r"#\[ignore\b")

# After the attribute, skip these kinds of lines while searching for the fn.
SKIP_RE = re.compile(r"^\s*(#\[|//|/\*|\*|$)")

# Capture the function name from `fn foo(` or `pub fn foo(` etc.
FN_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b")

RAW_START_RE = re.compile(r'r(?P<hashes>#*)"')

# Directories that never hold workspace sources.  Matched against the path
# RELATIVE to the scan root: the absolute path of the checkout may legitimately
# contain a component with one of these names (a worktree under `.../.claude/`
# or an artifact dir named `target`), and skipping on that would silently report
# an empty inventory instead of an error.
SKIP_DIRS = frozenset(
    {
        "target",
        ".git",
        ".cargo",
        "node_modules",
        ".extern-repos",
        ".venv",
        ".claude",
    }
)


def code_only(text: str) -> str:
    """Return `text` with comments and string/char literals blanked to spaces.

    Newlines are preserved so line numbers and columns stay truthful.  The
    scanner is a small Rust-aware state machine: nested block comments, line
    comments, normal strings with escapes, raw strings (any number of `#`) and
    char literals (including `'"'`, which otherwise opens a phantom string that
    swallows the rest of the file).
    """
    out = list(text)
    i = 0
    while i < len(text):
        end = _opaque_end(text, i)
        if end == i:
            i += 1
            continue
        _blank(out, i, end)
        i = end
    return "".join(out)


def _opaque_end(text: str, i: int) -> int:
    """Return the index just past the comment/literal at `i`, or `i` if none."""
    if text.startswith("//", i):
        return _line_comment_end(text, i)
    if text.startswith("/*", i):
        return _block_comment_end(text, i)
    if text[i] == '"':
        return _normal_string_end(text, i)
    if text[i] == "'":
        return _char_literal_end(text, i) or i
    if text[i] == "r":
        return _raw_string_end(text, i)
    return i


def _line_comment_end(text: str, start: int) -> int:
    end = text.find("\n", start)
    return len(text) if end < 0 else end


def _block_comment_end(text: str, start: int) -> int:
    """Nested `/* ... */` blocks are legal Rust; depth-count them."""
    depth = 0
    i = start
    while i < len(text):
        if text.startswith("/*", i):
            depth += 1
            i += 2
        elif text.startswith("*/", i):
            depth -= 1
            i += 2
            if depth == 0:
                return i
        else:
            i += 1
    return len(text)


def _normal_string_end(text: str, start: int) -> int:
    i = start + 1
    while i < len(text):
        if text[i] == "\\":
            i += 2
            continue
        if text[i] == '"':
            return i + 1
        i += 1
    return len(text)


def _raw_string_end(text: str, start: int) -> int:
    m = RAW_START_RE.match(text, start)
    if m is None:
        return start
    closing = '"' + m.group("hashes")
    end = text.find(closing, m.end())
    return len(text) if end < 0 else end + len(closing)


def _blank(out: list[str], start: int, end: int) -> None:
    for k in range(start, end):
        if out[k] != "\n":
            out[k] = " "


def _char_literal_end(text: str, start: int) -> int | None:
    """Return the index just past a char literal at `start`, else None.

    `'a'` and `'\\n'` are literals; `'a` in `&'a str` is a lifetime and is left
    alone.  `'"'` must be treated as a literal, otherwise the quote inside it
    opens a string that blanks the remainder of the line.
    """
    if text.startswith("'\\", start):
        # `'\''`, `'\n'`, `'\u{1F600}'` — scan to the closing quote.
        j = start + 2
        while j < len(text) and j < start + 12:
            if text[j] == "\\":
                j += 2
                continue
            if text[j] == "'":
                return j + 1
            j += 1
        return None
    if start + 2 < len(text) and text[start + 2] == "'":
        return start + 3
    return None


def _skip_spaces(text: str, i: int) -> int:
    while i < len(text) and text[i] in " \t":
        i += 1
    return i


def _string_value(text: str, i: int) -> tuple[str, int] | None:
    """Parse a normal or raw string literal at `i`; return (value, next index)."""
    m = RAW_START_RE.match(text, i)
    if m is not None:
        closing = '"' + m.group("hashes")
        end = text.find(closing, m.end())
        if end < 0:
            return None
        return text[m.end() : end], end + len(closing)
    if i < len(text) and text[i] == '"':
        j = i + 1
        while j < len(text):
            if text[j] == "\\":
                j += 2
                continue
            if text[j] == '"':
                return text[i + 1 : j], j + 1
            j += 1
    return None


def parse_ignore_attribute(line: str, start: int) -> str | None:
    """Parse a complete `#[ignore ...]` attribute starting at `start`.

    Returns the reason (``""`` for a bare `#[ignore]`), or None when the text at
    `start` is not a self-contained ignore attribute on this line.  A trailing
    `// comment` is allowed; anything else after `]` means this is not the
    attribute's line and the caller must not record it.
    """
    m = ATTRIBUTE_RE.match(line, start)
    if m is None:
        return None
    i = _skip_spaces(line, m.end())
    reason = ""
    if i < len(line) and line[i] == "=":
        parsed = _string_value(line, _skip_spaces(line, i + 1))
        if parsed is None:
            return None
        reason, i = parsed
        i = _skip_spaces(line, i)
    if i >= len(line) or line[i] != "]":
        return None
    rest = line[i + 1 :].strip()
    if rest and not rest.startswith("//"):
        return None
    return reason


def iter_rust_files(root: Path) -> Iterator[Path]:
    """Yield every `*.rs` file under `root`, skipping vendored/build dirs."""
    for path in root.rglob("*.rs"):
        relative_dirs = path.relative_to(root).parts[:-1]
        if any(part in SKIP_DIRS for part in relative_dirs):
            continue
        yield path


def find_ignored_tests(path: Path, workspace_root: Path) -> Iterator[dict]:
    """Yield one dict per `#[ignore]` attribute found in `path`.

    Each dict has keys: file, test_name, reason, line.
    """
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return
    lines = text.splitlines()
    blanked = code_only(text).splitlines()
    rel = path.relative_to(workspace_root).as_posix()
    for index, (raw_line, code_line) in enumerate(zip(lines, blanked)):
        start = len(code_line) - len(code_line.lstrip())
        reason = parse_ignore_attribute(raw_line, start)
        if reason is None or not code_line[start:].startswith("#[ignore"):
            continue
        # Find the next `fn` declaration, skipping comments / other attrs.
        test_name = ""
        for nxt in blanked[index + 1 :]:
            fn_m = FN_RE.match(nxt)
            if fn_m:
                test_name = fn_m.group("name")
                break
            if SKIP_RE.match(nxt):
                continue
            # Hit something that's neither attr/comment/blank nor fn — give up
            # on naming this one. Likely a macro invocation or odd layout.
            break
        yield {
            "file": rel,
            "test_name": test_name,
            "reason": reason,
            "line": index + 1,
        }


def collect_inventory(workspace_root: Path) -> list[dict]:
    findings: list[dict] = []
    for path in iter_rust_files(workspace_root):
        findings.extend(find_ignored_tests(path, workspace_root))
    findings.sort(key=lambda d: (d["file"], d["line"]))
    return findings


def emit_csv(findings: list[dict], out: io.TextIOBase) -> None:
    writer = csv.DictWriter(
        out, fieldnames=["file", "test_name", "reason", "line"], lineterminator="\n"
    )
    writer.writeheader()
    for row in findings:
        writer.writerow(row)


def emit_markdown(findings: list[dict], out: io.TextIOBase) -> None:
    total = len(findings)
    missing_reason = sum(1 for f in findings if not f["reason"])
    out.write("# Ignored-test inventory\n\n")
    out.write(
        "Auto-generated by `ci/audit_ignored_tests.py` "
        "via `.github/workflows/audit-ignored-tests.yml` (weekly cron).\n\n"
    )
    out.write(f"- **Total `#[ignore]` findings:** {total}\n")
    out.write(f"- **Missing reason string (policy violation per #839):** {missing_reason}\n\n")
    out.write("See [#839](https://github.com/FastLED/fbuild/issues/839) for the policy.\n\n")
    out.write("| File | Test | Reason | Line |\n")
    out.write("| --- | --- | --- | --- |\n")
    for f in findings:
        # Escape pipes in the reason to keep the table well-formed.
        reason = f["reason"].replace("|", "\\|") if f["reason"] else "_(no reason — policy violation)_"
        test = f["test_name"] or "_(unparseable)_"
        out.write(f"| `{f['file']}` | `{test}` | {reason} | {f['line']} |\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Audit #[ignore] attribute usage in the fbuild workspace."
    )
    parser.add_argument(
        "--workspace-root",
        default=".",
        type=Path,
        help="Workspace root to scan (default: current dir).",
    )
    fmt = parser.add_mutually_exclusive_group()
    fmt.add_argument("--csv", action="store_true", help="Emit CSV (default).")
    fmt.add_argument("--markdown", action="store_true", help="Emit markdown.")
    args = parser.parse_args(argv)

    workspace_root = args.workspace_root.resolve()
    findings = collect_inventory(workspace_root)
    if args.markdown:
        emit_markdown(findings, sys.stdout)
    else:
        emit_csv(findings, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())

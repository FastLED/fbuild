"""Reject production USB VID/PID identities outside FastLED/boards.

The scan covers the complete tracked tree. Explicit test paths and Rust items
guarded by ``#[cfg(test)]`` are permitted because production builds cannot
reach them. Documentation and this guard's own fixtures are non-runtime input.
"""

from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

TEXT_SUFFIXES = {
    ".c",
    ".cc",
    ".cpp",
    ".h",
    ".hpp",
    ".ini",
    ".js",
    ".json",
    ".md",
    ".ps1",
    ".py",
    ".rs",
    ".sh",
    ".toml",
    ".ts",
    ".txt",
    ".yaml",
    ".yml",
}

EXACT_EXCLUSIONS = {
    "ci/check_usb_vidpid_literals.py",
    "ci/test_check_usb_vidpid_literals.py",
}
TEST_PATH_PREFIXES = (
    "ci/docker-test-serial/",
    "crates/fbuild-core/data/",
)

IDENTITY_STRING_RE = re.compile(
    r"(?i)\b(?:0x)?([0-9a-f]{4})\s*:\s*(?:0x)?([0-9a-f]{4})\b"
)
TUPLE_PAIR_RE = re.compile(
    r"(?i)[\[(]\s*0x([0-9a-f]{4})\s*,\s*0x([0-9a-f]{4})\s*[\])]"
)
NAMED_LITERAL_RE = re.compile(
    r"(?i)\b(?:vid|pid|[a-z0-9]+_(?:vid|pid))\b\s*"
    r"(?::\s*[a-z_][a-z0-9_:<>]*)?\s*(?:==|!=|=|:)\s*"
    r"(?:Some\(\s*)?\b(0x[0-9a-f]{4}|[1-9][0-9]{0,4})\b"
)
JSON_FIELD_RE = re.compile(
    r'(?i)"(vid|pid)"\s*:\s*"?(0x[0-9a-f]{4}|[0-9a-f]{4}|[0-9]{1,5})"?'
)
EMBED_RE = re.compile(
    r'(?i)include_(?:bytes|str)!\s*\([^\n)]*(?:usb[^\n)]*(?:ids|vid|pid)|(?:ids|vid|pid)[^\n)]*usb)'
)
CATALOGUE_RE = re.compile(
    r"(?i)\b(?:board_fingerprints|environment_to_vcom|mcu_to_vid|seed_mcu|usb_vid_pid_catalog)\b"
)
CFG_TEST_RE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    reason: str
    excerpt: str


def test_only_path(path: str) -> bool:
    normalized = path.replace("\\", "/")
    if normalized in EXACT_EXCLUSIONS or Path(normalized).suffix.lower() == ".md":
        return True
    if normalized.startswith(TEST_PATH_PREFIXES):
        return True
    parts = normalized.split("/")
    name = parts[-1]
    return (
        "tests" in parts
        or name.startswith(("test_", "tests_"))
        or name in {"test.rs", "tests.rs"}
        or name.endswith("_test.rs")
    )


def _code_braces(
    line: str, in_block_comment: bool, raw_hashes: int | None
) -> tuple[int, bool, int | None]:
    """Count braces outside strings/comments on one Rust line."""
    delta = 0
    i = 0
    quote: str | None = None
    escaped = False
    while i < len(line):
        if raw_hashes is not None:
            delimiter = '"' + ('#' * raw_hashes)
            end = line.find(delimiter, i)
            if end < 0:
                return delta, in_block_comment, raw_hashes
            i = end + len(delimiter)
            raw_hashes = None
            continue
        if in_block_comment:
            end = line.find("*/", i)
            if end < 0:
                return delta, True, raw_hashes
            i = end + 2
            in_block_comment = False
            continue
        if quote is not None:
            char = line[i]
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            i += 1
            continue
        if line.startswith("//", i):
            break
        if line.startswith("/*", i):
            in_block_comment = True
            i += 2
            continue
        raw_match = re.match(r'r(#{0,16})"', line[i:])
        if raw_match:
            raw_hashes = len(raw_match.group(1))
            i += len(raw_match.group(0))
            continue
        if line[i] == '"':
            quote = line[i]
        elif line[i] == "{":
            delta += 1
        elif line[i] == "}":
            delta -= 1
        i += 1
    return delta, in_block_comment, raw_hashes


def strip_cfg_test_items(source: str) -> str:
    """Blank Rust items selected by a standalone ``#[cfg(test)]`` attribute."""
    lines = source.splitlines(keepends=True)
    output = list(lines)
    pending = False
    skipping = False
    saw_body = False
    depth = 0
    in_block_comment = False
    raw_hashes: int | None = None

    for index, line in enumerate(lines):
        if not pending and not skipping and CFG_TEST_RE.search(line):
            output[index] = "\n" if line.endswith("\n") else ""
            pending = True
            remainder = CFG_TEST_RE.sub("", line).strip()
            if not remainder:
                continue
            line = remainder

        if pending or skipping:
            output[index] = "\n" if lines[index].endswith("\n") else ""
            stripped = line.strip()
            if pending and (not stripped or stripped.startswith("#[")):
                continue
            pending = False
            skipping = True
            delta, in_block_comment, raw_hashes = _code_braces(
                line, in_block_comment, raw_hashes
            )
            depth += delta
            saw_body = saw_body or delta > 0 or "{" in line
            if (saw_body and depth <= 0) or (not saw_body and ";" in line):
                skipping = False
                saw_body = False
                depth = 0
                in_block_comment = False
                raw_hashes = None

    return "".join(output)


def scan_text(path: str, source: str) -> list[Finding]:
    if test_only_path(path):
        return []
    if path.endswith(".rs"):
        source = strip_cfg_test_items(source)

    findings: list[Finding] = []
    seen: set[tuple[int, str]] = set()
    for line_number, line in enumerate(source.splitlines(), 1):
        stripped = line.strip()
        if not stripped or stripped.startswith(("//", "//!", "///", "# ")):
            continue
        checks = (
            (JSON_FIELD_RE, "VID/PID field"),
            (NAMED_LITERAL_RE, "named VID/PID literal"),
            (TUPLE_PAIR_RE, "USB-shaped numeric pair"),
            (IDENTITY_STRING_RE, "USB-shaped identity string"),
            (EMBED_RE, "embedded USB identity asset"),
            (CATALOGUE_RE, "legacy USB catalogue symbol"),
        )
        for pattern, reason in checks:
            if not pattern.search(line):
                continue
            key = (line_number, reason)
            if key not in seen:
                findings.append(Finding(path, line_number, reason, stripped[:160]))
                seen.add(key)
    return findings


def tracked_paths() -> list[str]:
    result = subprocess.run(
        ["git", "ls-files", "-z"], check=True, capture_output=True
    ).stdout
    return [item.decode("utf-8") for item in result.split(b"\0") if item]


def scan_tree(root: Path = Path(".")) -> list[Finding]:
    findings: list[Finding] = []
    for path in tracked_paths():
        file_path = root / path
        if not file_path.is_file() or file_path.suffix.lower() not in TEXT_SUFFIXES:
            continue
        try:
            source = file_path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        findings.extend(scan_text(path, source))
    return findings


def main() -> int:
    findings = scan_tree()
    if findings:
        print(
            "production USB VID/PID data is forbidden; publish it through "
            "FastLED/boards and consume the verified artifact:",
            file=sys.stderr,
        )
        for finding in findings:
            print(
                f"  {finding.path}:{finding.line}: {finding.reason}: "
                f"{finding.excerpt}",
                file=sys.stderr,
            )
        return 1
    print("Full-tree USB VID/PID source guard passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

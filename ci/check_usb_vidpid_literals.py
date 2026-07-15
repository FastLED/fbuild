"""Guard the preparatory USB VID/PID diff surface.

This check currently covers only added same-line pairs in ``crates/`` and
``python/``, plus separate ``vid``/``pid`` fields in the board JSON snapshot.
The broader deny-all cleanup is deferred until FastLED/boards#47 lands.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys

PAIR_RE = re.compile(r"0x([0-9a-fA-F]{4})\s*[:,/]\s*0x([0-9a-fA-F]{4})")
PAIR_STRING_RE = re.compile(r"(?i)\b([0-9a-f]{4}):([0-9a-f]{4})\b")
JSON_FIELD_RE = re.compile(
    r'"(?P<field>vid|pid)"\s*:\s*"?(?P<value>0x[0-9a-fA-F]{4}|[0-9a-fA-F]{4})"?',
    re.IGNORECASE,
)
EXCLUDED_PARTS = (
    "/tests/",
    "\\tests\\",
    "/test_",
    "\\test_",
    "/docs/",
    "\\docs\\",
    "online-data-tools/",
    "online-data-tools\\",
)


def production_path(path: str) -> bool:
    normalized = path.replace("\\", "/")
    if any(part in normalized for part in EXCLUDED_PARTS):
        return False
    return normalized.startswith(("crates/", "python/"))


def added_production_pairs(diff: str) -> list[tuple[str, str, str]]:
    path = ""
    findings: list[tuple[str, str, str]] = []
    for line in diff.splitlines():
        if line.startswith("+++ b/"):
            path = line[6:]
            continue
        if not line.startswith("+") or line.startswith("+++") or not production_path(path):
            continue
        normalized = path.replace("\\", "/")
        matches = (*PAIR_RE.finditer(line), *PAIR_STRING_RE.finditer(line))
        if normalized.startswith("crates/fbuild-config/assets/boards/json/"):
            matches = (*matches, *JSON_FIELD_RE.finditer(line))
        for match in matches:
            if len(match.groups()) == 2 and match.groupdict().get("field"):
                findings.append((path, match.group("field").upper(), match.group("value")))
                continue
            if len(match.groups()) == 1:
                findings.append((path, match.group(1).upper(), "????"))
                continue
            findings.append((path, match.group(1).upper(), match.group(2).upper()))
    return findings


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="origin/main")
    args = parser.parse_args()
    diff = subprocess.run(
        ["git", "diff", "--unified=0", f"{args.base}...HEAD", "--"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    findings = added_production_pairs(diff)
    if findings:
        print(
            "new same-line crates/python or board-JSON USB VID/PID literals "
            "are not allowed:",
            file=sys.stderr,
        )
        for path, vid, pid in findings:
            print(f"  {path}: {vid}:{pid}", file=sys.stderr)
        print(
            "Publish the identity through FastLED/boards and consume its artifact. "
            "This preparatory guard does not cover separate Rust constants, "
            "decimal/symbolic forms, non-board JSON, CI/setup/workflow/root paths, "
            "or row-level board provenance.",
            file=sys.stderr,
        )
        return 1
    print(
        "USB VID/PID diff guard passed for same-line crates/python pairs and "
        "board JSON vid/pid fields."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

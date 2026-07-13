#!/usr/bin/env python3
"""Fail when a Dylint allowlist names a path absent from the repository."""

from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    missing: list[str] = []
    for allowlist in sorted((ROOT / "dylints").glob("*/src/allowlist.txt")):
        for number, raw in enumerate(allowlist.read_text(encoding="utf-8").splitlines(), 1):
            path = raw.split("#", 1)[0].strip()
            if not path:
                continue
            if not (ROOT / path).is_file():
                missing.append(f"{allowlist.relative_to(ROOT)}:{number} -> missing {path}")
    if missing:
        print("stale Dylint allowlist paths:", file=sys.stderr)
        print("\n".join(missing), file=sys.stderr)
        return 1
    print("Dylint allowlist paths are current.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

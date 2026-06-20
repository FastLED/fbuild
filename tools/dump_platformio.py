#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["platformio>=6"]
# ///
"""Dump every board PlatformIO knows about as a sorted JSON map.

Invokes `pio boards --json-output`, which returns the full catalog
(~1900+ boards across ~40 platforms) — no per-platform installation
required, PlatformIO queries its own registry. Output is normalized
into a `{board_id: {...board metadata...}}` map sorted alphabetically
by id so diffs on the `online-data` branch are reviewable.

Usage:
    uv run --no-project --script dump_platformio.py [OUTPUT_PATH]

  - OUTPUT_PATH omitted → stream JSON to stdout (workflow can `>` redirect).
  - OUTPUT_PATH supplied → write to that file (must NOT exist as a dir).

The script intentionally never overwrites previously-committed data
itself — it just produces a fresh dump. The companion `merge_pio_boards.py`
script is what reconciles new + old to preserve fields the new dump
would otherwise drop.

Exit codes:
  0 — success, valid JSON produced.
  1 — `pio boards` failed or returned non-JSON. **Caller (the workflow)
      must treat this as a non-fatal source failure and keep the old
      committed `data/pio-boards.json` untouched.**

Schema produced (per-board):
    {
      "<board_id>": {
        "id":         "<board_id>",           # echoed for self-describing rows
        "name":       "Human readable name",
        "platform":   "ststm32" | "espressif32" | ...,
        "mcu":        "STM32F412ZGT6",
        "fcpu":       100000000,
        "ram":        262144,
        "rom":        1048576,
        "frameworks": ["arduino", "cmsis", ...],
        "vendor":     "ST",
        "url":        "https://...",
        "connectivity": ["can", "wifi"],      # optional, only when set upstream
        "debug":      { ... },                # optional
        ...                                   # any other field upstream emits
      },
      ...
    }
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


def dump_boards() -> dict[str, dict]:
    """Run `pio boards --json-output` and normalize the result."""
    proc = subprocess.run(
        ["pio", "boards", "--json-output"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    if proc.returncode != 0:
        print(f"pio boards failed (exit {proc.returncode}):", file=sys.stderr)
        print(proc.stderr, file=sys.stderr)
        raise SystemExit(1)
    try:
        raw = json.loads(proc.stdout)
    except json.JSONDecodeError as e:
        print(f"pio boards returned non-JSON output: {e}", file=sys.stderr)
        # First 500 chars to help diagnose
        print(proc.stdout[:500], file=sys.stderr)
        raise SystemExit(1)
    if not isinstance(raw, list):
        print(
            f"pio boards returned unexpected top-level type {type(raw).__name__}; "
            "expected a list of board objects.",
            file=sys.stderr,
        )
        raise SystemExit(1)

    out: dict[str, dict] = {}
    for entry in raw:
        if not isinstance(entry, dict):
            continue
        board_id = entry.get("id")
        if not isinstance(board_id, str) or not board_id:
            continue
        out[board_id] = entry
    return out


def main() -> int:
    argv = sys.argv[1:]
    if len(argv) > 1:
        print(f"usage: dump_platformio.py [OUTPUT_PATH]", file=sys.stderr)
        return 2

    boards = dump_boards()
    if not boards:
        print("pio boards returned 0 boards; refusing to emit an empty dump", file=sys.stderr)
        return 1

    sorted_boards = {k: boards[k] for k in sorted(boards)}
    payload = json.dumps(sorted_boards, indent=2, ensure_ascii=False, sort_keys=True) + "\n"

    if argv:
        out_path = Path(argv[0])
        if out_path.is_dir():
            print(f"refusing to overwrite directory {out_path}", file=sys.stderr)
            return 2
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(payload, encoding="utf-8")
        print(
            f"wrote {len(sorted_boards)} boards to {out_path}",
            file=sys.stderr,
        )
    else:
        sys.stdout.write(payload)
        print(f"wrote {len(sorted_boards)} boards to stdout", file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())

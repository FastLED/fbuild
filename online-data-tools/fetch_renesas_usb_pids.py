#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch ArduinoCore-renesas USB PID rows into merge_sources.py JSON.

Renesas does not appear to publish a public USB PID allocation registry for
Arduino RA boards. The repo-supported Renesas platform boards are Arduino
boards, and ArduinoCore-renesas carries their `boards.txt` VID/PID rows.

This is intentionally a weak board-package supplement: it fills missing
Arduino product names after first-party and generic USB-ID sources have had a
chance to win. It does not override Renesas-owned VID 0x045b rows.

Output schema:
    {
      "2341:0069": {
        "vendor": "Arduino SA",
        "product": "Arduino UNO R4 Minima"
      }
    }
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.request
from collections import OrderedDict
from pathlib import Path
from typing import Callable

ARDUINO_CORE_RENESAS_BOARDS_URL = (
    "https://raw.githubusercontent.com/arduino/ArduinoCore-renesas/main/boards.txt"
)

ARDUINO_VENDORS = {
    "2341": "Arduino SA",
    # Historical Arduino.org VID. ArduinoCore-renesas currently emits only
    # 0x2341 rows, but accepting 0x2a03 keeps the parser future-compatible
    # without assigning these rows to Renesas.
    "2a03": "dog hunter AG",
}

_BOARD_NAME_RE = re.compile(r"^(?P<board>[A-Za-z0-9_]+)\.name=(?P<name>.+)$")
_BOARD_VID_RE = re.compile(
    r"^(?P<board>[A-Za-z0-9_]+)\.(?:vid\.(?P<index_a>\d+)|"
    r"upload_port\.(?P<index_b>\d+)\.vid)="
    r"0x(?P<vid>[0-9A-Fa-f]{4})$"
)
_BOARD_PID_RE = re.compile(
    r"^(?P<board>[A-Za-z0-9_]+)\.(?:pid\.(?P<index_a>\d+)|"
    r"upload_port\.(?P<index_b>\d+)\.pid)="
    r"0x(?P<pid>[0-9A-Fa-f]{4})$"
)


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def _normalize_product_name(name: str) -> str:
    return re.sub(r"\s+", " ", name).strip()


def _collapse_board_products(names: list[str]) -> str:
    unique = sorted(set(_normalize_product_name(name) for name in names))
    if len(unique) == 1:
        return unique[0]
    return " / ".join(unique)


def parse_boards_txt(text: str) -> dict[str, dict[str, str]]:
    """Parse Arduino-style VID/PID declarations from `boards.txt`."""
    board_names: dict[str, str] = {}
    vids: dict[tuple[str, str], str] = {}
    pids: dict[tuple[str, str], str] = {}

    for raw_line in text.splitlines():
        line = raw_line.strip()
        name_match = _BOARD_NAME_RE.match(line)
        if name_match:
            board_names[name_match.group("board")] = name_match.group("name").strip()
            continue
        vid_match = _BOARD_VID_RE.match(line)
        if vid_match:
            key = (
                vid_match.group("board"),
                vid_match.group("index_a") or vid_match.group("index_b"),
            )
            vids[key] = vid_match.group("vid").lower()
            continue
        pid_match = _BOARD_PID_RE.match(line)
        if pid_match:
            key = (
                pid_match.group("board"),
                pid_match.group("index_a") or pid_match.group("index_b"),
            )
            pids[key] = pid_match.group("pid").lower()

    names_by_vidpid: dict[str, list[str]] = {}
    for key, vid in vids.items():
        if vid not in ARDUINO_VENDORS or key not in pids:
            continue
        board_name = board_names.get(key[0])
        if board_name is None:
            continue
        names_by_vidpid.setdefault(f"{vid}:{pids[key]}", []).append(board_name)

    return {
        vidpid: {
            "vendor": ARDUINO_VENDORS[vidpid.split(":", 1)[0]],
            "product": _collapse_board_products(names),
        }
        for vidpid, names in sorted(names_by_vidpid.items())
    }


def collect(
    *,
    fetch: Callable[[str], str] = _fetch,
    url: str = ARDUINO_CORE_RENESAS_BOARDS_URL,
) -> dict[str, dict[str, str]]:
    try:
        text = fetch(url)
    except Exception as e:
        print(f"warning: {url}: fetch failed: {e}", file=sys.stderr)
        return {}

    entries = parse_boards_txt(text)
    print(f"ArduinoCore-renesas boards: {url}: {len(entries)} PID(s)", file=sys.stderr)
    return dict(sorted(entries.items()))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()

    entries = collect()
    args.out.write_text(
        json.dumps(OrderedDict(sorted(entries.items())), indent=2, ensure_ascii=False)
        + "\n",
        encoding="utf-8",
    )
    print(f"wrote {args.out}: {len(entries)} Renesas/Arduino PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

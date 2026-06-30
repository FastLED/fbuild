#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch Adafruit USB PID rows into merge_sources.py JSON.

Adafruit does not publish a single public USB PID registry. Its authoritative
public rows are spread across:

* Adafruit-maintained Arduino `boards.txt` files.
* TinyUF2 bootloader `board.h` descriptors.
* CircuitPython `mpconfigboard.mk` descriptors for Adafruit-prefixed boards.

Arduino rows are merged first. TinyUF2 and CircuitPython rows fill newer
ESP32/RP2040 gaps without replacing Arduino-core names for existing PIDs.

Output schema:
    {
      "239a:801b": {
        "vendor": "Adafruit",
        "product": "Adafruit Feather M0 Express (SAMD21)"
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
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable

ADAFRUIT_VENDOR = "Adafruit"
ADAFRUIT_VID = "239a"


@dataclass(frozen=True)
class BoardSource:
    name: str
    url: str


ADAFRUIT_ARDUINO_SOURCES = (
    BoardSource(
        "Adafruit ArduinoCore-samd",
        "https://raw.githubusercontent.com/adafruit/ArduinoCore-samd/master/boards.txt",
    ),
    BoardSource(
        "Adafruit nRF52 Arduino",
        "https://raw.githubusercontent.com/adafruit/Adafruit_nRF52_Arduino/master/boards.txt",
    ),
    BoardSource(
        "Adafruit Arduino Boards",
        "https://raw.githubusercontent.com/adafruit/Adafruit_Arduino_Boards/master/boards.txt",
    ),
    BoardSource(
        "Adafruit WICED Arduino",
        "https://raw.githubusercontent.com/adafruit/Adafruit_WICED_Arduino/master/boards.txt",
    ),
)

TINYUF2_BRANCH = "master"
TINYUF2_TREE_URL = (
    f"https://api.github.com/repos/adafruit/tinyuf2/git/trees/{TINYUF2_BRANCH}"
    "?recursive=1"
)
TINYUF2_RAW_BASE = (
    f"https://raw.githubusercontent.com/adafruit/tinyuf2/{TINYUF2_BRANCH}"
)

CIRCUITPYTHON_BRANCH = "main"
CIRCUITPYTHON_TREE_URL = (
    "https://api.github.com/repos/adafruit/circuitpython/git/trees/"
    f"{CIRCUITPYTHON_BRANCH}?recursive=1"
)
CIRCUITPYTHON_RAW_BASE = (
    f"https://raw.githubusercontent.com/adafruit/circuitpython/{CIRCUITPYTHON_BRANCH}"
)

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
_C_DEFINE_RE = re.compile(
    r'^\s*#define\s+(?P<name>USB_(?:VID|PID|MANUFACTURER|PRODUCT))\s+'
    r'(?P<value>0x[0-9A-Fa-f]{4}|"[^"]+")\s*$',
    re.M,
)
_MAKE_RE = re.compile(
    r'^\s*(?P<name>USB_(?:VID|PID|MANUFACTURER|PRODUCT))\s*=\s*'
    r'(?P<value>0x[0-9A-Fa-f]{4}|"[^"]+")\s*$',
    re.M,
)


def _fetch_text(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def _fetch_json(url: str) -> dict:
    return json.loads(_fetch_text(url))


def _hex4(value: str) -> str:
    text = value.strip().lower()
    if text.startswith("0x"):
        text = text[2:]
    return f"{int(text, 16):04x}"


def _string_value(value: str) -> str:
    value = value.strip()
    if value.startswith('"') and value.endswith('"'):
        return value[1:-1]
    return value


def _normalize_product_name(name: str) -> str:
    return re.sub(r"\s+", " ", name).strip()


def _full_product_name(manufacturer: str, product: str) -> str:
    manufacturer = _normalize_product_name(manufacturer)
    product = _normalize_product_name(product)
    if product.lower().startswith(manufacturer.lower()):
        return product
    return f"{manufacturer} {product}"


def _collapse_products(names: list[str]) -> str:
    unique = sorted(set(_normalize_product_name(name) for name in names))
    if len(unique) == 1:
        return unique[0]
    return " / ".join(unique)


def _merge_fill_gaps(
    base: dict[str, dict[str, str]],
    supplement: dict[str, dict[str, str]],
) -> dict[str, dict[str, str]]:
    out = dict(base)
    for key, value in sorted(supplement.items()):
        out.setdefault(key, value)
    return out


def parse_boards_txt(text: str) -> dict[str, dict[str, str]]:
    """Parse Adafruit VID rows from Arduino-style `boards.txt`."""
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
        if vid != ADAFRUIT_VID or key not in pids:
            continue
        board_name = board_names.get(key[0])
        if board_name is None:
            continue
        names_by_vidpid.setdefault(f"{vid}:{pids[key]}", []).append(board_name)

    return {
        vidpid: {
            "vendor": ADAFRUIT_VENDOR,
            "product": _collapse_products(names),
        }
        for vidpid, names in sorted(names_by_vidpid.items())
    }


def _parse_assignments(text: str, pattern: re.Pattern[str]) -> dict[str, str]:
    return {
        match.group("name"): _string_value(match.group("value"))
        for match in pattern.finditer(text)
    }


def parse_usb_descriptor_text(text: str, *, syntax: str) -> dict[str, dict[str, str]]:
    """Parse TinyUF2 `board.h` or CircuitPython `mpconfigboard.mk` USB rows."""
    pattern = _C_DEFINE_RE if syntax == "c" else _MAKE_RE
    values = _parse_assignments(text, pattern)
    required = {"USB_VID", "USB_PID", "USB_MANUFACTURER", "USB_PRODUCT"}
    if not required <= set(values):
        return {}

    vid = _hex4(values["USB_VID"])
    if vid != ADAFRUIT_VID:
        return {}
    manufacturer = values["USB_MANUFACTURER"]
    if not manufacturer.lower().startswith("adafruit"):
        return {}

    pid = _hex4(values["USB_PID"])
    product = _full_product_name(manufacturer, values["USB_PRODUCT"])
    return {
        f"{vid}:{pid}": {
            "vendor": ADAFRUIT_VENDOR,
            "product": product,
        }
    }


def _tree_paths(tree_payload: dict) -> list[str]:
    tree = tree_payload.get("tree")
    if not isinstance(tree, list):
        return []
    paths = []
    for item in tree:
        if isinstance(item, dict) and isinstance(item.get("path"), str):
            paths.append(item["path"])
    return paths


def _tinyuf2_board_paths(tree_payload: dict) -> list[str]:
    return [
        path
        for path in _tree_paths(tree_payload)
        if path.endswith("/board.h")
        and "/boards/adafruit_" in path
    ]


def _circuitpython_board_paths(tree_payload: dict) -> list[str]:
    return [
        path
        for path in _tree_paths(tree_payload)
        if path.endswith("/mpconfigboard.mk")
        and "/boards/adafruit_" in path
    ]


def collect_arduino_boards(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    sources: Iterable[BoardSource] = ADAFRUIT_ARDUINO_SOURCES,
) -> dict[str, dict[str, str]]:
    entries: dict[str, dict[str, str]] = {}
    for source in sources:
        try:
            text = fetch_text(source.url)
        except Exception as e:
            print(f"warning: {source.name}: {source.url}: fetch failed: {e}", file=sys.stderr)
            continue
        rows = parse_boards_txt(text)
        entries = _merge_fill_gaps(entries, rows)
        print(f"{source.name}: {len(rows)} PID(s)", file=sys.stderr)
    return dict(sorted(entries.items()))


def collect_tinyuf2(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_json: Callable[[str], dict] = _fetch_json,
    tree_url: str = TINYUF2_TREE_URL,
    raw_base: str = TINYUF2_RAW_BASE,
) -> dict[str, dict[str, str]]:
    try:
        paths = _tinyuf2_board_paths(fetch_json(tree_url))
    except Exception as e:
        print(f"warning: tinyuf2 tree fetch failed: {e}", file=sys.stderr)
        return {}

    entries: dict[str, dict[str, str]] = {}
    for path in paths:
        try:
            text = fetch_text(f"{raw_base}/{path}")
        except Exception as e:
            print(f"warning: tinyuf2 {path}: fetch failed: {e}", file=sys.stderr)
            continue
        entries = _merge_fill_gaps(
            entries,
            parse_usb_descriptor_text(text, syntax="c"),
        )
    print(f"TinyUF2 Adafruit descriptors: {len(entries)} PID(s)", file=sys.stderr)
    return dict(sorted(entries.items()))


def collect_circuitpython(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_json: Callable[[str], dict] = _fetch_json,
    tree_url: str = CIRCUITPYTHON_TREE_URL,
    raw_base: str = CIRCUITPYTHON_RAW_BASE,
) -> dict[str, dict[str, str]]:
    try:
        paths = _circuitpython_board_paths(fetch_json(tree_url))
    except Exception as e:
        print(f"warning: circuitpython tree fetch failed: {e}", file=sys.stderr)
        return {}

    entries: dict[str, dict[str, str]] = {}
    for path in paths:
        try:
            text = fetch_text(f"{raw_base}/{path}")
        except Exception as e:
            print(f"warning: circuitpython {path}: fetch failed: {e}", file=sys.stderr)
            continue
        entries = _merge_fill_gaps(
            entries,
            parse_usb_descriptor_text(text, syntax="make"),
        )
    print(f"CircuitPython Adafruit descriptors: {len(entries)} PID(s)", file=sys.stderr)
    return dict(sorted(entries.items()))


def collect(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_json: Callable[[str], dict] = _fetch_json,
) -> dict[str, dict[str, str]]:
    entries = collect_arduino_boards(fetch_text=fetch_text)
    entries = _merge_fill_gaps(
        entries,
        collect_tinyuf2(fetch_text=fetch_text, fetch_json=fetch_json),
    )
    entries = _merge_fill_gaps(
        entries,
        collect_circuitpython(fetch_text=fetch_text, fetch_json=fetch_json),
    )
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
    print(f"wrote {args.out}: {len(entries)} Adafruit PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

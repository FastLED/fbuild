#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Extract USB VID:PID rows from fbuild's checked-in board JSON files.

This is a repo-scope supplement for the USB resolver. It only considers board
definitions under `crates/fbuild-config/assets/boards/json` and emits the flat
JSON shape consumed by `online-data/tools/merge_sources.py`:

    {
      "239a:811b": {
        "vendor": "Adafruit",
        "product": "Adafruit Feather ESP32-S3 (2MB PSRAM)"
      }
    }

The workflow orders this source after stronger vendor/generic USB tables, so
it fills product-name gaps for fbuild-supported boards without replacing
better USB-owner data.
"""

from __future__ import annotations

import argparse
import json
import re
from collections import OrderedDict
from pathlib import Path


def _hex4(value: object) -> str | None:
    if value is None:
        return None
    if isinstance(value, int):
        number = value
    else:
        text = str(value).strip().lower()
        if text.startswith("0x"):
            text = text[2:]
        if not text:
            return None
        try:
            number = int(text, 16)
        except ValueError:
            return None
    if not 0 <= number <= 0xFFFF:
        return None
    return f"{number:04x}"


def _clean(text: object) -> str:
    return re.sub(r"\s+", " ", str(text)).strip()


def _collapse(values: list[str]) -> str:
    unique = sorted({value for value in (_clean(value) for value in values) if value})
    return " / ".join(unique)


def extract_board_usb_pids(boards_dir: Path) -> dict[str, dict[str, str]]:
    rows: dict[str, dict[str, list[str]]] = {}
    for path in sorted(boards_dir.glob("*.json")):
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if not isinstance(data, dict):
            continue
        build = data.get("build")
        if not isinstance(build, dict):
            continue
        vid = _hex4(build.get("vid"))
        pid = _hex4(build.get("pid"))
        if vid is None or pid is None:
            continue

        product = _clean(data.get("name") or data.get("id") or path.stem)
        if not product:
            continue
        vendor = _clean(data.get("vendor")) or f"Unknown vendor 0x{vid.upper()}"
        key = f"{vid}:{pid}"
        bucket = rows.setdefault(key, {"vendors": [], "products": []})
        bucket["vendors"].append(vendor)
        bucket["products"].append(product)

    out: dict[str, dict[str, str]] = {}
    for key, values in rows.items():
        out[key] = {
            "vendor": _collapse(values["vendors"]),
            "product": _collapse(values["products"]),
        }
    return OrderedDict(sorted(out.items()))


def write_json(path: Path, rows: dict[str, dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as f:
        json.dump(rows, f, indent=2, ensure_ascii=False, sort_keys=True)
        f.write("\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--boards-dir",
        type=Path,
        default=Path("crates/fbuild-config/assets/boards/json"),
        help="Directory containing fbuild board JSON files",
    )
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    write_json(args.out, extract_board_usb_pids(args.boards_dir))


if __name__ == "__main__":
    main()

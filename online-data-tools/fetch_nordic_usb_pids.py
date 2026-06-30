#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch Nordic Semiconductor USB PID rows into merge_sources.py JSON.

Nordic does not publish a standalone PID allocation table like Espressif or
Raspberry Pi. The maintained nRF Connect Programmer source carries the current
Nordic USB DFU / MCUboot product ID lists, and `pc-nrf-dfu-js` documents the
pre-programmed nRF52840 dongle USB SDFU bootloader PID.

Output schema:
    {
      "1915:521f": {
        "vendor": "Nordic Semiconductor ASA",
        "product": "PCA10059 nRF52840 Dongle USB SDFU bootloader"
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

NORDIC_VENDOR = "Nordic Semiconductor ASA"
NORDIC_VID = "1915"

PROGRAMMER_DEVICES_URL = (
    "https://raw.githubusercontent.com/nordicsemi/pc-nrfconnect-programmer/"
    "main/src/util/devices.ts"
)
DFU_README_URL = (
    "https://raw.githubusercontent.com/NordicSemiconductor/pc-nrf-dfu-js/"
    "master/README.md"
)

_ARRAY_RE_TEMPLATE = r"export\s+const\s+{name}\s*=\s*\[(?P<body>.*?)\];"
_HEX_RE = re.compile(r"0x(?P<hex>[0-9A-Fa-f]{4})")
_NORDIC_VENDOR_RE = re.compile(r"NORDIC_SEMICONDUCTOR\s*=\s*0x(?P<vid>[0-9A-Fa-f]{4})")
_SPACE_RE = re.compile(r"\s+")

_COMMENT_PRODUCTS = {
    "Thingy91": "Nordic Thingy:91",
    "Thingy53": "Nordic Thingy:53",
    "nPM1300": "Nordic nPM1300",
    "nPM1300-Serial-Recovery": "Nordic nPM1300 Serial Recovery",
}


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def _clean_comment(raw: str) -> str:
    return _SPACE_RE.sub(" ", raw.strip())


def _comment_product(comment: str | None, *, fallback: str) -> str:
    if not comment:
        return fallback
    return _COMMENT_PRODUCTS.get(comment, f"Nordic {comment}")


def parse_array_entries(text: str, array_name: str) -> list[tuple[str, str | None]]:
    """Return `(pid, nearest_comment)` entries from a TypeScript hex array."""
    array_re = re.compile(_ARRAY_RE_TEMPLATE.format(name=re.escape(array_name)), re.S)
    m = array_re.search(text)
    if not m:
        return []

    entries: list[tuple[str, str | None]] = []
    current_comment: str | None = None
    for line in m.group("body").splitlines():
        comment_part = line.split("//", 1)
        if len(comment_part) == 2:
            current_comment = _clean_comment(comment_part[1])
        for pid_match in _HEX_RE.finditer(comment_part[0]):
            entries.append((pid_match.group("hex").lower(), current_comment))
    return entries


def parse_programmer_devices(text: str) -> dict[str, str]:
    """Parse Nordic's Programmer device source into `{pid: product}`."""
    m = _NORDIC_VENDOR_RE.search(text)
    if not m or m.group("vid").lower() != NORDIC_VID:
        raise ValueError("Nordic Programmer source does not declare VID 0x1915")

    out: dict[str, str] = {}
    for pid, _comment in parse_array_entries(text, "USBProductIds"):
        out[pid] = "Nordic USB serial DFU"

    for pid, comment in parse_array_entries(text, "McubootProductIds"):
        out.setdefault(
            pid,
            _comment_product(comment, fallback="Nordic USB MCUboot"),
        )

    for pid, comment in parse_array_entries(text, "ModemProductIds"):
        out.setdefault(
            pid,
            _comment_product(comment, fallback="Nordic USB modem"),
        )

    return out


def parse_dfu_readme(text: str) -> dict[str, str]:
    """Return product-name overrides proven by the DFU README."""
    lowered = text.lower()
    if "0x1915" not in lowered or "0x521f" not in lowered:
        return {}
    if "pca10059" not in lowered or "nrf52840 dongle" not in lowered:
        return {}
    return {"521f": "PCA10059 nRF52840 Dongle USB SDFU bootloader"}


def collect(
    *,
    fetch: Callable[[str], str] = _fetch,
    programmer_url: str = PROGRAMMER_DEVICES_URL,
    dfu_readme_url: str = DFU_README_URL,
) -> dict[str, dict[str, str]]:
    """Fetch all Nordic PID sources and emit merge-compatible entries."""
    try:
        programmer_text = fetch(programmer_url)
    except Exception as e:
        print(f"warning: {programmer_url}: fetch failed: {e}", file=sys.stderr)
        return {}

    pid_to_product = parse_programmer_devices(programmer_text)
    print(
        f"nordic programmer devices: {programmer_url}: "
        f"{len(pid_to_product)} PID(s)",
        file=sys.stderr,
    )

    try:
        readme_text = fetch(dfu_readme_url)
    except Exception as e:
        print(f"warning: {dfu_readme_url}: fetch failed: {e}", file=sys.stderr)
    else:
        overrides = parse_dfu_readme(readme_text)
        pid_to_product.update(overrides)
        print(
            f"nordic dfu readme: {dfu_readme_url}: "
            f"{len(overrides)} override(s)",
            file=sys.stderr,
        )

    return {
        f"{NORDIC_VID}:{pid}": {
            "vendor": NORDIC_VENDOR,
            "product": product,
        }
        for pid, product in sorted(pid_to_product.items())
    }


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
    print(f"wrote {args.out}: {len(entries)} Nordic PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

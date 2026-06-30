#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch Espressif's official USB PID registry into merge_sources.py JSON.

Espressif maintains the public `espressif/usb-pids` registry for products
using VID 0x303A. The nightly online-data workflow consumes the output before
the generic USB-ID merge so PID-level names land in `usb-vid.json`, the www
SQLite `vidpid` table, and downstream VID:PID lookups.

Output schema:
    {
      "303a:8001": {
        "vendor": "Espressif Systems",
        "product": "Unexpected Maker TinyS2 - Arduino"
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
from typing import Callable, Iterable

ESPRESSIF_VENDOR = "Espressif Systems"
ESPRESSIF_VID = "303a"

CUSTOMER_PIDS_URL = (
    "https://raw.githubusercontent.com/espressif/usb-pids/main/allocated-pids.txt"
)
DEVBOARD_PIDS_URL = (
    "https://raw.githubusercontent.com/espressif/usb-pids/main/"
    "allocated-pids-espressif-devboards.txt"
)

# Espressif-owned common PIDs documented outside the usb-pids registry.
BUILTIN_PIDS: dict[str, str] = {
    "0002": "ESP32-S2 USB-OTG",
    "1001": "USB JTAG/serial debug unit",
    "4001": "ESP-IDF TinyUSB serial device",
}

_PID_ROW_RE = re.compile(
    r"^\s*(?:0x)?(?P<pid>[0-9A-Fa-f]{4})\s*\|\s*(?P<name>.*?)\s*$"
)
_SPACE_RE = re.compile(r"\s+")


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def _clean_product_name(raw: str) -> str:
    return _SPACE_RE.sub(" ", raw.strip())


def parse_pid_registry(text: str) -> dict[str, str]:
    """Parse Espressif `PID | Product name` text into `{pid: product}`.

    Malformed rows, headings, blank names, and explicit unallocated rows are
    ignored. Duplicate PIDs with different names are rejected because the
    upstream registry should be internally unambiguous.
    """
    out: dict[str, str] = {}
    for line in text.splitlines():
        m = _PID_ROW_RE.match(line)
        if not m:
            continue
        pid = m.group("pid").lower()
        product = _clean_product_name(m.group("name"))
        if not product or product.lower() == "unallocated":
            continue
        previous = out.get(pid)
        if previous is not None and previous != product:
            raise ValueError(
                f"duplicate Espressif PID 0x{pid}: {previous!r} vs {product!r}"
            )
        out[pid] = product
    return out


def collect(
    *,
    fetch: Callable[[str], str] = _fetch,
    urls: Iterable[str] = (DEVBOARD_PIDS_URL, CUSTOMER_PIDS_URL),
) -> dict[str, dict[str, str]]:
    """Fetch all Espressif PID sources and emit merge-compatible entries.

    Built-ins are loaded first, then official devboard and customer registry
    rows fill the rest. If a registry row repeats a built-in PID, the built-in
    name stays authoritative.
    """
    pid_to_product: dict[str, str] = dict(BUILTIN_PIDS)
    for url in urls:
        try:
            text = fetch(url)
        except Exception as e:
            print(f"warning: {url}: fetch failed: {e}", file=sys.stderr)
            continue
        parsed = parse_pid_registry(text)
        for pid, product in parsed.items():
            pid_to_product.setdefault(pid, product)
        print(f"espressif usb-pids: {url}: {len(parsed)} PID(s)", file=sys.stderr)

    return {
        f"{ESPRESSIF_VID}:{pid}": {
            "vendor": ESPRESSIF_VENDOR,
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
    print(f"wrote {args.out}: {len(entries)} Espressif PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

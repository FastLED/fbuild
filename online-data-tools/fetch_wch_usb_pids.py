#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch WCH CH34x/CH91xx USB serial PID rows into merge_sources.py JSON.

WCH's newer CH343 Linux driver carries product IDs for CH342/CH343/CH344/
CH346/CH347/CH339/CH910x/CH911x/CH9433 USB serial chips. The udev rules give
commented product names for the older subset; newer driver-only rows are named
from the driver's PID-to-chip switch cases. Board names are intentionally not
inferred from bridge-chip IDs.

Output schema:
    {
      "1a86:55d4": {
        "vendor": "QinHeng Electronics",
        "product": "WCH CH9102 USB/Serial converter"
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

WCH_VENDOR = "QinHeng Electronics"
WCH_VID = "1a86"

CH343_DRIVER_URL = (
    "https://raw.githubusercontent.com/WCHSoftGroup/ch343ser_linux/"
    "main/driver/ch343.c"
)
CH343_UDEV_URL = (
    "https://raw.githubusercontent.com/WCHSoftGroup/ch343ser_linux/"
    "main/udev/99-ch34x.rules"
)

_USB_ID_RE = re.compile(
    r"USB_DEVICE(?:_INTERFACE_NUMBER)?\(\s*0x(?P<vid>[0-9A-Fa-f]{4})\s*,\s*"
    r"0x(?P<pid>[0-9A-Fa-f]{4})"
)
_UDEV_ID_RE = re.compile(
    r'ATTRS\{idVendor\}=="(?P<vid>[0-9A-Fa-f]{4})".*'
    r'ATTRS\{idProduct\}=="(?P<pid>[0-9A-Fa-f]{4})"'
)
_COMMENT_RE = re.compile(r"^#\s*(?P<product>WCH\s+.+?)\s*$")

# Product names for driver-supported rows not present in WCH's udev comments.
# These are derived from the `switch (ch343->idProduct)` chiptype assignments
# in the same driver source.
DRIVER_ONLY_PRODUCTS = {
    "55e7": "WCH CH339 USB/Serial converter",
    "55e8": "WCH CH9114 USB/Serial converter",
    "55e9": "WCH CH9111 Mode0 USB/Serial converter",
    "55ea": "WCH CH9111 Mode1 USB/Serial converter",
    "55eb": "WCH CH346C Mode0/Mode1 USB/Serial converter",
    "55ec": "WCH CH346C Mode2 USB/Serial converter",
    "55ef": "WCH CH9105 USB/Serial converter",
    "5610": "WCH CH9433 USB/Serial converter",
}


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def parse_driver_pids(text: str) -> set[str]:
    """Parse WCH VID product IDs from the CH343 driver ID table."""
    pids: set[str] = set()
    for match in _USB_ID_RE.finditer(text):
        if match.group("vid").lower() == WCH_VID:
            pids.add(match.group("pid").lower())
    return pids


def parse_udev_products(text: str) -> dict[str, str]:
    """Parse `{pid: product}` rows from WCH's commented udev rules."""
    out: dict[str, str] = {}
    current_product: str | None = None
    for line in text.splitlines():
        comment = _COMMENT_RE.match(line.strip())
        if comment:
            current_product = comment.group("product")
            continue
        match = _UDEV_ID_RE.search(line)
        if not match:
            continue
        if match.group("vid").lower() != WCH_VID or current_product is None:
            continue
        pid = match.group("pid").lower()
        previous = out.get(pid)
        if previous is not None and previous != current_product:
            raise ValueError(f"duplicate WCH PID 0x{pid}: {previous!r} vs {current_product!r}")
        out[pid] = current_product
    return out


def build_supplement(
    *,
    driver_pids: set[str],
    udev_products: dict[str, str],
) -> dict[str, str]:
    """Combine udev names and driver-only chip labels into `{pid: product}`."""
    missing_from_driver = sorted(set(udev_products) - driver_pids)
    if missing_from_driver:
        raise ValueError(f"WCH udev rows missing from driver table: {missing_from_driver}")

    missing_driver_only = sorted(set(DRIVER_ONLY_PRODUCTS) - driver_pids)
    if missing_driver_only:
        raise ValueError(
            f"WCH driver missing expected newer product rows: {missing_driver_only}"
        )

    out = dict(udev_products)
    for pid, product in DRIVER_ONLY_PRODUCTS.items():
        out.setdefault(pid, product)
    return dict(sorted(out.items()))


def collect(
    *,
    fetch: Callable[[str], str] = _fetch,
    driver_url: str = CH343_DRIVER_URL,
    udev_url: str = CH343_UDEV_URL,
) -> dict[str, dict[str, str]]:
    """Fetch WCH sources and emit merge-compatible entries."""
    try:
        driver_text = fetch(driver_url)
    except Exception as e:
        print(f"warning: {driver_url}: fetch failed: {e}", file=sys.stderr)
        return {}

    try:
        udev_text = fetch(udev_url)
    except Exception as e:
        print(f"warning: {udev_url}: fetch failed: {e}", file=sys.stderr)
        return {}

    driver_pids = parse_driver_pids(driver_text)
    udev_products = parse_udev_products(udev_text)
    pid_to_product = build_supplement(
        driver_pids=driver_pids,
        udev_products=udev_products,
    )
    skipped = sorted(driver_pids - set(pid_to_product))
    if skipped:
        print(
            f"wch ch343 driver: skipped undocumented PID(s): {', '.join(skipped)}",
            file=sys.stderr,
        )
    print(
        f"wch ch343 sources: {len(pid_to_product)} PID(s)",
        file=sys.stderr,
    )
    return {
        f"{WCH_VID}:{pid}": {
            "vendor": WCH_VENDOR,
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
    print(f"wrote {args.out}: {len(entries)} WCH PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

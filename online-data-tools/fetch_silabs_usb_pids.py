#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch Silicon Labs / Energy Micro USB PID rows into merge_sources.py JSON.

Silicon Labs does not publish a Raspberry-Pi-style PID allocation registry.
This supplement uses primary source-backed rows only:

* Linux's CP210x driver proves current Silicon Labs-owned bridge default PIDs
  under VID 0x10C4. Product labels are conservative chip-family names.
* A first-party SiliconLabsSoftware Arduino example documents the Energy Micro
  VID 0x2544 / PID 0x0001 udev rule required for OpenOCD access.

Board names are intentionally not inferred from bridge-chip or debug-interface
IDs; board-package VID/PID rows for Arduino, Seeed, SparkFun, etc. belong to
their vendor-specific fetchers.

Output schema:
    {
      "10c4:ea71": {
        "vendor": "Silicon Labs",
        "product": "CP2108 Quad UART Bridge"
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

SILABS_VENDOR = "Silicon Labs"
SILABS_VID = "10c4"
ENERGY_MICRO_VENDOR = "Energy Micro AS"
ENERGY_MICRO_VID = "2544"

CP210X_DRIVER_URL = (
    "https://raw.githubusercontent.com/torvalds/linux/master/"
    "drivers/usb/serial/cp210x.c"
)
SILABS_OPENOCD_README_URL = (
    "https://raw.githubusercontent.com/SiliconLabsSoftware/"
    "devs-arduino-ble-motor-control/main/README.md"
)

CP210X_PRODUCTS = {
    "ea60": "CP210x UART Bridge",
    "ea61": "CP210x UART Bridge",
    "ea63": "CP210x UART Bridge",
    "ea70": "CP2105 Dual UART Bridge",
    "ea71": "CP2108 Quad UART Bridge",
    "ea7a": "CP2105 Dual UART Bridge",
    "ea7b": "CP2108 Quad UART Bridge",
}

ENERGY_MICRO_PRODUCTS = {
    "0001": "Silicon Labs OpenOCD debug interface",
}

_USB_DEVICE_RE = re.compile(
    r"USB_DEVICE(?:_INTERFACE_NUMBER)?\(\s*0x(?P<vid>[0-9A-Fa-f]{4})\s*,\s*"
    r"0x(?P<pid>[0-9A-Fa-f]{4})"
)
_UDEV_ID_RE = re.compile(
    r'ATTRS\{idVendor\}=="(?P<vid>[0-9A-Fa-f]{4})".*'
    r'ATTRS\{idProduct\}=="(?P<pid>[0-9A-Fa-f]{4})"'
)


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def parse_cp210x_driver_pids(text: str) -> set[str]:
    """Parse Silicon Labs VID product IDs from Linux's CP210x driver."""
    pids: set[str] = set()
    for match in _USB_DEVICE_RE.finditer(text):
        if match.group("vid").lower() == SILABS_VID:
            pids.add(match.group("pid").lower())
    return pids


def parse_energy_micro_udev_pids(text: str) -> set[str]:
    """Parse Energy Micro VID product IDs from udev-rule snippets."""
    pids: set[str] = set()
    for match in _UDEV_ID_RE.finditer(text):
        if match.group("vid").lower() == ENERGY_MICRO_VID:
            pids.add(match.group("pid").lower())
    return pids


def build_cp210x_supplement(driver_pids: set[str]) -> dict[str, str]:
    """Return selected Silicon Labs CP210x defaults as `{pid: product}`."""
    missing = sorted(set(CP210X_PRODUCTS) - driver_pids)
    if missing:
        raise ValueError(f"Linux CP210x driver missing expected PIDs: {missing}")
    return dict(sorted(CP210X_PRODUCTS.items()))


def build_energy_micro_supplement(udev_pids: set[str]) -> dict[str, str]:
    """Return selected Energy Micro / Silicon Labs rows as `{pid: product}`."""
    missing = sorted(set(ENERGY_MICRO_PRODUCTS) - udev_pids)
    if missing:
        raise ValueError(
            f"SiliconLabsSoftware udev snippet missing expected PIDs: {missing}"
        )
    return dict(sorted(ENERGY_MICRO_PRODUCTS.items()))


def collect(
    *,
    fetch: Callable[[str], str] = _fetch,
    cp210x_url: str = CP210X_DRIVER_URL,
    openocd_readme_url: str = SILABS_OPENOCD_README_URL,
) -> dict[str, dict[str, str]]:
    """Fetch Silicon Labs sources and emit merge-compatible entries."""
    entries: dict[str, dict[str, str]] = {}

    try:
        cp210x_text = fetch(cp210x_url)
    except Exception as e:
        print(f"warning: {cp210x_url}: fetch failed: {e}", file=sys.stderr)
    else:
        pid_to_product = build_cp210x_supplement(
            parse_cp210x_driver_pids(cp210x_text)
        )
        entries.update(
            {
                f"{SILABS_VID}:{pid}": {
                    "vendor": SILABS_VENDOR,
                    "product": product,
                }
                for pid, product in pid_to_product.items()
            }
        )
        print(
            f"linux cp210x: {cp210x_url}: {len(pid_to_product)} PID(s)",
            file=sys.stderr,
        )

    try:
        readme_text = fetch(openocd_readme_url)
    except Exception as e:
        print(f"warning: {openocd_readme_url}: fetch failed: {e}", file=sys.stderr)
    else:
        pid_to_product = build_energy_micro_supplement(
            parse_energy_micro_udev_pids(readme_text)
        )
        entries.update(
            {
                f"{ENERGY_MICRO_VID}:{pid}": {
                    "vendor": ENERGY_MICRO_VENDOR,
                    "product": product,
                }
                for pid, product in pid_to_product.items()
            }
        )
        print(
            f"silabs openocd udev: {openocd_readme_url}: "
            f"{len(pid_to_product)} PID(s)",
            file=sys.stderr,
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
    print(f"wrote {args.out}: {len(entries)} Silicon Labs PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch PJRC/Teensy USB PID rows into merge_sources.py JSON.

Teensyduino assigns several VID 0x16C0 product IDs by USB personality rather
than by board model. The PJRC core headers carry the current PID/product-name
pairs; `teensy_loader_cli` documents the rebootor and HalfKay bootloader PIDs.

Some PIDs are shared across variants that differ by USB BCD version, which the
flat online-data schema cannot represent. Those rows are collapsed to a
conservative product-family name instead of choosing a misleading single mode.

Output schema:
    {
      "16c0:04d5": {
        "vendor": "Van Ooijen Technische Informatica",
        "product": "Teensyduino MTP Disk + Serial"
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

TEENSY_VENDOR = "Van Ooijen Technische Informatica"
TEENSY_VID = "16c0"

TEENSY3_USB_DESC_URL = (
    "https://raw.githubusercontent.com/PaulStoffregen/cores/master/"
    "teensy3/usb_desc.h"
)
TEENSY4_USB_DESC_URL = (
    "https://raw.githubusercontent.com/PaulStoffregen/cores/master/"
    "teensy4/usb_desc.h"
)
TEENSY_LOADER_CLI_URL = (
    "https://raw.githubusercontent.com/PaulStoffregen/teensy_loader_cli/"
    "master/teensy_loader_cli.c"
)

_MODE_RE = re.compile(r"#(?:el)?if\s+defined\((?P<mode>USB_[A-Za-z0-9_]+)\)")
_DEFINE_HEX_RE = re.compile(
    r"#define\s+(?P<name>VENDOR_ID|PRODUCT_ID)\s+0x(?P<value>[0-9A-Fa-f]{4})"
)
_PRODUCT_NAME_RE = re.compile(r"#define\s+PRODUCT_NAME\s+\{(?P<body>.*?)\}")
_CHAR_RE = re.compile(r"'(?P<char>(?:\\'|[^'])*)'")

LOADER_PRODUCTS = {
    "0477": "Teensy Rebootor",
    "0478": "Teensy HalfKay Bootloader",
}

PID_PRODUCT_COLLAPSE = {
    "0485": "Teensyduino MIDI",
    "0488": "Teensyduino Flight Sim Controls",
    "0489": "Teensyduino MIDI + Serial",
    "048a": "Teensyduino MIDI/Audio + Serial",
    "04d5": "Teensyduino MTP Disk + Serial",
}


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def parse_product_name(line: str) -> str | None:
    """Parse a C char-array `PRODUCT_NAME` define into a Python string."""
    m = _PRODUCT_NAME_RE.search(line)
    if not m:
        return None
    chars = []
    for char_match in _CHAR_RE.finditer(m.group("body")):
        value = char_match.group("char")
        chars.append("'" if value == "\\'" else value)
    return "".join(chars) if chars else None


def _flush_block(
    out: dict[str, set[str]],
    *,
    vendor_id: str | None,
    product_id: str | None,
    product_name: str | None,
) -> None:
    if vendor_id != TEENSY_VID or product_id is None or product_name is None:
        return
    out.setdefault(product_id, set()).add(product_name)


def parse_usb_desc(text: str) -> dict[str, set[str]]:
    """Parse PJRC `usb_desc.h` rows into `{pid: {product names}}`."""
    out: dict[str, set[str]] = {}
    vendor_id: str | None = None
    product_id: str | None = None
    product_name: str | None = None
    in_mode = False

    for line in text.splitlines():
        if _MODE_RE.search(line):
            _flush_block(
                out,
                vendor_id=vendor_id,
                product_id=product_id,
                product_name=product_name,
            )
            vendor_id = None
            product_id = None
            product_name = None
            in_mode = True
            continue

        if in_mode and line.lstrip().startswith("#endif"):
            _flush_block(
                out,
                vendor_id=vendor_id,
                product_id=product_id,
                product_name=product_name,
            )
            vendor_id = None
            product_id = None
            product_name = None
            in_mode = False
            continue

        define = _DEFINE_HEX_RE.search(line)
        if define:
            value = define.group("value").lower()
            if define.group("name") == "VENDOR_ID":
                vendor_id = value
            else:
                product_id = value
            continue

        name = parse_product_name(line)
        if name is not None:
            product_name = name

    _flush_block(
        out,
        vendor_id=vendor_id,
        product_id=product_id,
        product_name=product_name,
    )
    return out


def parse_loader_products(text: str) -> dict[str, str]:
    """Return loader PID labels if `teensy_loader_cli` still references them."""
    out: dict[str, str] = {}
    for pid, product in LOADER_PRODUCTS.items():
        if f"0x{TEENSY_VID.upper()}, 0x{pid.upper()}" in text:
            out[pid] = product
    return out


def collapse_products(pid_to_names: dict[str, set[str]]) -> dict[str, str]:
    """Collapse duplicate PID product-name variants to one flat label."""
    out: dict[str, str] = {}
    for pid, names in sorted(pid_to_names.items()):
        if pid in PID_PRODUCT_COLLAPSE:
            out[pid] = PID_PRODUCT_COLLAPSE[pid]
            continue
        if len(names) > 1:
            joined = ", ".join(sorted(names))
            raise ValueError(f"Teensy PID 0x{pid} has unhandled names: {joined}")
        out[pid] = normalize_product_name(next(iter(names)))
    return out


def normalize_product_name(name: str) -> str:
    """Make PJRC descriptor names self-contained for a VID:PID table."""
    if name.startswith(("Teensy", "Teensyduino")):
        return name
    if name == "USB Serial":
        return "Teensyduino Serial"
    return f"Teensyduino {name}"


def collect(
    *,
    fetch: Callable[[str], str] = _fetch,
    usb_desc_urls: Iterable[str] = (TEENSY3_USB_DESC_URL, TEENSY4_USB_DESC_URL),
    loader_url: str = TEENSY_LOADER_CLI_URL,
) -> dict[str, dict[str, str]]:
    """Fetch PJRC sources and emit merge-compatible entries."""
    pid_to_names: dict[str, set[str]] = {}
    for url in usb_desc_urls:
        try:
            text = fetch(url)
        except Exception as e:
            print(f"warning: {url}: fetch failed: {e}", file=sys.stderr)
            continue
        parsed = parse_usb_desc(text)
        for pid, names in parsed.items():
            pid_to_names.setdefault(pid, set()).update(names)
        print(f"teensy usb_desc: {url}: {len(parsed)} PID(s)", file=sys.stderr)

    pid_to_product = collapse_products(pid_to_names)

    try:
        loader_text = fetch(loader_url)
    except Exception as e:
        print(f"warning: {loader_url}: fetch failed: {e}", file=sys.stderr)
    else:
        loader_products = parse_loader_products(loader_text)
        pid_to_product.update(loader_products)
        print(
            f"teensy loader: {loader_url}: {len(loader_products)} PID(s)",
            file=sys.stderr,
        )

    return {
        f"{TEENSY_VID}:{pid}": {
            "vendor": TEENSY_VENDOR,
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
    print(f"wrote {args.out}: {len(entries)} Teensy PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

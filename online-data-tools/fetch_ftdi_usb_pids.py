#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch selected original FTDI USB PID rows into merge_sources.py JSON.

The upstream Linux `ftdi_sio_ids.h` header has two very different sections:
FTDI's original device PIDs followed by many third-party allocations using
FTDI's VID. This supplement parses only the original FTDI section and emits a
small allowlist of FTDI-owned rows that generic USB-ID sources commonly miss.

Output schema:
    {
      "0403:6040": {
        "vendor": "Future Technology Devices International, Ltd",
        "product": "FT2233HP Dual channel hi-speed device with PD"
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
from typing import Callable

FTDI_VENDOR = "Future Technology Devices International, Ltd"
FTDI_VID = "0403"
FTDI_SIO_IDS_URL = (
    "https://raw.githubusercontent.com/torvalds/linux/master/"
    "drivers/usb/serial/ftdi_sio_ids.h"
)

_VID_RE = re.compile(r"^#define\s+FTDI_VID\s+0x(?P<vid>[0-9A-Fa-f]{4})\b", re.M)
_DEFINE_RE = re.compile(
    r"^#define\s+(?P<name>FTDI_[A-Za-z0-9_]+_PID)\s+"
    r"0x(?P<pid>[0-9A-Fa-f]{4})"
    r"(?:\s*/\*\s*(?P<comment>.*?)\s*\*/)?",
    re.M,
)
_THIRD_PARTY_MARKER = "/*** third-party PIDs"

SUPPLEMENT_PRODUCTS = {
    "FTDI_8U232AM_ALT_PID": "8U232AM alternate PID",
    "FTDI_FT2233HP_PID": "FT2233HP Dual channel hi-speed device with PD",
    "FTDI_FT4233HP_PID": "FT4233HP Quad channel hi-speed device with PD",
    "FTDI_FT2232HP_PID": "FT2232HP Dual channel hi-speed device with PD",
    "FTDI_FT4232HP_PID": "FT4232HP Quad channel hi-speed device with PD",
    "FTDI_FT233HP_PID": "FT233HP Dual channel hi-speed device with PD",
    "FTDI_FT232HP_PID": "FT232HP Dual channel hi-speed device with PD",
    "FTDI_FT4232HA_PID": "FT4232HA Quad channel automotive grade hi-speed device",
    "FTDI_SIO_PID": "SIO application of 8U100AX",
    "FTDI_232RL_PID": "FT232RL",
}


@dataclass(frozen=True)
class FtdiDefine:
    name: str
    pid: str
    comment: str


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def parse_original_ftdi_defines(text: str) -> dict[str, FtdiDefine]:
    """Parse FTDI's original PID section from Linux `ftdi_sio_ids.h`."""
    m = _VID_RE.search(text)
    if not m or m.group("vid").lower() != FTDI_VID:
        raise ValueError("Linux FTDI header does not declare FTDI_VID 0x0403")

    original_section = text.split(_THIRD_PARTY_MARKER, 1)[0]
    out: dict[str, FtdiDefine] = {}
    for define in _DEFINE_RE.finditer(original_section):
        name = define.group("name")
        pid = define.group("pid").lower()
        comment = (define.group("comment") or "").strip()
        previous = out.get(name)
        if previous is not None and previous.pid != pid:
            raise ValueError(f"duplicate FTDI define {name}: {previous.pid} vs {pid}")
        out[name] = FtdiDefine(name=name, pid=pid, comment=comment)
    return out


def build_supplement(defines: dict[str, FtdiDefine]) -> dict[str, str]:
    """Return `{pid: product}` for the selected FTDI-owned supplement rows."""
    missing = sorted(set(SUPPLEMENT_PRODUCTS) - set(defines))
    if missing:
        raise ValueError(f"Linux FTDI header missing expected defines: {missing}")

    pid_to_product: dict[str, str] = {}
    for name, product in SUPPLEMENT_PRODUCTS.items():
        define = defines[name]
        previous = pid_to_product.get(define.pid)
        if previous is not None and previous != product:
            raise ValueError(
                f"duplicate FTDI PID 0x{define.pid}: {previous!r} vs {product!r}"
            )
        pid_to_product[define.pid] = product
    return pid_to_product


def collect(
    *,
    fetch: Callable[[str], str] = _fetch,
    url: str = FTDI_SIO_IDS_URL,
) -> dict[str, dict[str, str]]:
    """Fetch Linux's FTDI header and emit merge-compatible entries."""
    try:
        text = fetch(url)
    except Exception as e:
        print(f"warning: {url}: fetch failed: {e}", file=sys.stderr)
        return {}

    defines = parse_original_ftdi_defines(text)
    pid_to_product = build_supplement(defines)
    print(
        f"ftdi_sio_ids.h: {url}: {len(pid_to_product)} FTDI supplement PID(s)",
        file=sys.stderr,
    )
    return {
        f"{FTDI_VID}:{pid}": {
            "vendor": FTDI_VENDOR,
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
    print(f"wrote {args.out}: {len(entries)} FTDI PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

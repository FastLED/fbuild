#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch STMicroelectronics USB PID rows into merge_sources.py JSON.

The ST/OpenOCD ST-LINK driver carries the current ST-LINK debugger PID list,
including newer V3E/V3P products that generic USB-ID databases may lag. This
supplement emits debugger/function product rows only; it does not infer
Nucleo/Discovery board names from their onboard ST-LINK USB devices.

Output schema:
    {
      "0483:3757": {
        "vendor": "STMicroelectronics",
        "product": "STLINK-V3P"
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

STM_VENDOR = "STMicroelectronics"
STM_VID = "0483"

STLINK_USB_C_URL = (
    "https://raw.githubusercontent.com/STMicroelectronics/OpenOCD/master/"
    "src/jtag/drivers/stlink_usb.c"
)
STLINK_CFG_URL = (
    "https://raw.githubusercontent.com/STMicroelectronics/OpenOCD/master/"
    "tcl/interface/stlink.cfg"
)

_STLINK_DEFINE_RE = re.compile(
    r"^#define\s+(?P<name>STLINK_[A-Za-z0-9_]+_PID)\s+\(?0x(?P<pid>[0-9A-Fa-f]{4})\)?",
    re.M,
)
_CFG_PAIR_RE = re.compile(r"0x(?P<vid>[0-9A-Fa-f]{4})\s+0x(?P<pid>[0-9A-Fa-f]{4})")

STLINK_PRODUCTS = {
    "STLINK_V1_PID": "ST-LINK/V1",
    "STLINK_V2_PID": "ST-LINK/V2",
    "STLINK_V2_1_PID": "ST-LINK/V2.1",
    "STLINK_V2_1_NO_MSD_PID": "ST-LINK/V2.1 no MSD",
    "STLINK_V3_USBLOADER_PID": "STLINK-V3 USB loader",
    "STLINK_V3E_PID": "STLINK-V3E",
    "STLINK_V3S_PID": "STLINK-V3S",
    "STLINK_V3_2VCP_PID": "STLINK-V3 with 2 VCP",
    "STLINK_V3E_NO_MSD_PID": "STLINK-V3E no MSD",
    "STLINK_V3P_USBLOADER_PID": "STLINK-V3P USB loader",
    "STLINK_V3P_PID": "STLINK-V3P",
}

COMMON_ST_PRODUCTS = {
    "df11": "STM Device in DFU Mode",
    "5740": "Virtual COM Port",
}


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def parse_stlink_defines(text: str) -> dict[str, str]:
    """Parse `{macro_name: pid}` from ST's OpenOCD ST-LINK driver."""
    out: dict[str, str] = {}
    for match in _STLINK_DEFINE_RE.finditer(text):
        name = match.group("name")
        if name not in STLINK_PRODUCTS:
            continue
        pid = match.group("pid").lower()
        previous = out.get(name)
        if previous is not None and previous != pid:
            raise ValueError(f"duplicate ST-LINK define {name}: {previous} vs {pid}")
        out[name] = pid

    missing = sorted(set(STLINK_PRODUCTS) - set(out))
    if missing:
        raise ValueError(f"ST-LINK driver missing expected defines: {missing}")
    return out


def parse_stlink_cfg_pids(text: str) -> set[str]:
    """Parse ST VID product IDs from `stlink.cfg`."""
    pids: set[str] = set()
    for match in _CFG_PAIR_RE.finditer(text.replace("\\\n", " ")):
        if match.group("vid").lower() == STM_VID:
            pids.add(match.group("pid").lower())
    return pids


def build_supplement(
    *,
    defines: dict[str, str],
    cfg_pids: set[str],
) -> dict[str, str]:
    """Return `{pid: product}` for ST-LINK plus common ST USB function rows."""
    define_pids = set(defines.values())
    missing_from_cfg = sorted(define_pids - cfg_pids)
    if missing_from_cfg:
        raise ValueError(f"ST-LINK cfg missing driver PID(s): {missing_from_cfg}")

    out = dict(COMMON_ST_PRODUCTS)
    for name, pid in defines.items():
        product = STLINK_PRODUCTS[name]
        previous = out.get(pid)
        if previous is not None and previous != product:
            raise ValueError(f"duplicate ST PID 0x{pid}: {previous!r} vs {product!r}")
        out[pid] = product
    return dict(sorted(out.items()))


def collect(
    *,
    fetch: Callable[[str], str] = _fetch,
    driver_url: str = STLINK_USB_C_URL,
    cfg_url: str = STLINK_CFG_URL,
) -> dict[str, dict[str, str]]:
    """Fetch ST sources and emit merge-compatible entries."""
    try:
        driver_text = fetch(driver_url)
    except Exception as e:
        print(f"warning: {driver_url}: fetch failed: {e}", file=sys.stderr)
        return {}

    try:
        cfg_text = fetch(cfg_url)
    except Exception as e:
        print(f"warning: {cfg_url}: fetch failed: {e}", file=sys.stderr)
        return {}

    defines = parse_stlink_defines(driver_text)
    cfg_pids = parse_stlink_cfg_pids(cfg_text)
    pid_to_product = build_supplement(defines=defines, cfg_pids=cfg_pids)
    print(
        f"stm stlink sources: {len(pid_to_product)} PID(s)",
        file=sys.stderr,
    )
    return {
        f"{STM_VID}:{pid}": {
            "vendor": STM_VENDOR,
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
    print(f"wrote {args.out}: {len(entries)} STM PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

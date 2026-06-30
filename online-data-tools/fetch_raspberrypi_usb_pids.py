#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch Raspberry Pi's official USB PID registry into merge_sources.py JSON.

Raspberry Pi maintains the public `raspberrypi/usb-pid` registry for products
using VID 0x2E8A. The nightly online-data workflow consumes the output before
generic USB-ID sources so Raspberry Pi's allocation table wins PID-level name
conflicts.

Output schema:
    {
      "2e8a:0003": {
        "vendor": "Raspberry Pi Foundation",
        "product": "Raspberry Pi RP2040 boot"
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

RASPBERRY_PI_VENDOR = "Raspberry Pi Foundation"
RASPBERRY_PI_VID = "2e8a"
USB_PID_README_URL = (
    "https://raw.githubusercontent.com/raspberrypi/usb-pid/master/Readme.md"
)

_PID_RE = re.compile(r"^(?:0x)?(?P<pid>[0-9A-Fa-f]{4})$")
_LINK_RE = re.compile(r"\[([^\]]+)\]\([^)]+\)")
_SPACE_RE = re.compile(r"\s+")


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def _clean_markdown_cell(raw: str) -> str:
    text = raw.replace("<br>", " ").replace("<br/>", " ").replace("<br />", " ")
    text = _LINK_RE.sub(r"\1", text)
    text = text.replace("**", "").replace("__", "").replace("`", "")
    return _SPACE_RE.sub(" ", text.strip())


def _split_markdown_row(line: str) -> list[str] | None:
    stripped = line.strip()
    if not stripped.startswith("|") or not stripped.endswith("|"):
        return None
    return [_clean_markdown_cell(cell) for cell in stripped.strip("|").split("|")]


def parse_pid_table(text: str) -> dict[str, str]:
    """Parse Raspberry Pi's Markdown PID table into `{pid: product}`.

    Only concrete Product ID rows with a non-empty product description are
    emitted. Section headings, allocation ranges, blank placeholders, and
    explicit reserved rows are ignored.
    """
    out: dict[str, str] = {}
    for line in text.splitlines():
        cells = _split_markdown_row(line)
        if cells is None or len(cells) < 3:
            continue
        pid_cell, company, product = cells[:3]
        m = _PID_RE.match(pid_cell)
        if not m:
            continue
        if not product:
            continue
        if company.lower().startswith("reserved") or product.lower().startswith(
            "reserved"
        ):
            continue

        pid = m.group("pid").lower()
        previous = out.get(pid)
        if previous is not None and previous != product:
            raise ValueError(
                f"duplicate Raspberry Pi PID 0x{pid}: {previous!r} vs {product!r}"
            )
        out[pid] = product
    return out


def collect(
    *,
    fetch: Callable[[str], str] = _fetch,
    url: str = USB_PID_README_URL,
) -> dict[str, dict[str, str]]:
    """Fetch Raspberry Pi's PID table and emit merge-compatible entries."""
    try:
        text = fetch(url)
    except Exception as e:
        print(f"warning: {url}: fetch failed: {e}", file=sys.stderr)
        return {}

    parsed = parse_pid_table(text)
    print(f"raspberrypi usb-pid: {url}: {len(parsed)} PID(s)", file=sys.stderr)
    return {
        f"{RASPBERRY_PI_VID}:{pid}": {
            "vendor": RASPBERRY_PI_VENDOR,
            "product": product,
        }
        for pid, product in sorted(parsed.items())
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
    print(f"wrote {args.out}: {len(entries)} Raspberry Pi PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

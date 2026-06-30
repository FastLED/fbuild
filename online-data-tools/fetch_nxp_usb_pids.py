#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch NXP mfgtools/UUU ROM-loader PID rows into merge_sources.py JSON.

NXP's mfgtools (`uuu`) source carries the current VID 0x1FC9 ROM downloader
and fastboot PID table for i.MX / i.MX RT devices. These rows identify NXP
boot ROM or protocol modes, not board models.

Output schema:
    {
      "1fc9:0135": {
        "vendor": "NXP Semiconductors",
        "product": "NXP MXRT106X serial downloader (SDP)"
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

NXP_VENDOR = "NXP Semiconductors"
NXP_VID = "1fc9"
MFGTOOLS_CONFIG_URL = (
    "https://raw.githubusercontent.com/nxp-imx/mfgtools/master/libuuu/config.cpp"
)

_NXP_VID_RE = re.compile(r"constexpr\s+uint16_t\s+NXP_VID\s*=\s*0x(?P<vid>[0-9A-Fa-f]{4})")
_CONFIG_RE = re.compile(
    r'emplace_back\(ConfigItem\{\s*"(?P<protocol>[^"]+)"\s*,\s*'
    r'(?P<name>"[^"]+"|nullptr)\s*,\s*'
    r'(?P<alias>"[^"]+"|nullptr)\s*,\s*NXP_VID\s*,\s*'
    r"0x(?P<pid>[0-9A-Fa-f]{4})"
)


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def _unquote(value: str) -> str | None:
    if value == "nullptr":
        return None
    return value.strip('"')


def product_name(*, protocol: str, name: str | None) -> str:
    """Build a conservative product name from an NXP ConfigItem row."""
    clean_protocol = protocol.rstrip(":")
    if clean_protocol in {"SDP", "SDPS", "SDPV"}:
        target = name or clean_protocol
        return f"NXP {target} serial downloader ({clean_protocol})"
    if clean_protocol == "FBK":
        return "NXP fastboot kernel"
    if clean_protocol == "FB":
        return "NXP fastboot"
    target = name or clean_protocol
    return f"NXP {target} ({clean_protocol})"


def parse_mfgtools_config(text: str) -> dict[str, str]:
    """Parse NXP mfgtools `config.cpp` into `{pid: product}`."""
    m = _NXP_VID_RE.search(text)
    if not m or m.group("vid").lower() != NXP_VID:
        raise ValueError("mfgtools config does not declare NXP_VID 0x1fc9")

    out: dict[str, str] = {}
    for match in _CONFIG_RE.finditer(text):
        protocol = match.group("protocol")
        name = _unquote(match.group("name"))
        pid = match.group("pid").lower()
        product = product_name(protocol=protocol, name=name)
        previous = out.get(pid)
        if previous is not None and previous != product:
            raise ValueError(f"duplicate NXP PID 0x{pid}: {previous!r} vs {product!r}")
        out[pid] = product
    return dict(sorted(out.items()))


def collect(
    *,
    fetch: Callable[[str], str] = _fetch,
    url: str = MFGTOOLS_CONFIG_URL,
) -> dict[str, dict[str, str]]:
    """Fetch NXP mfgtools config and emit merge-compatible entries."""
    try:
        text = fetch(url)
    except Exception as e:
        print(f"warning: {url}: fetch failed: {e}", file=sys.stderr)
        return {}

    pid_to_product = parse_mfgtools_config(text)
    print(f"nxp mfgtools: {url}: {len(pid_to_product)} PID(s)", file=sys.stderr)
    return {
        f"{NXP_VID}:{pid}": {
            "vendor": NXP_VENDOR,
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
    print(f"wrote {args.out}: {len(entries)} NXP PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

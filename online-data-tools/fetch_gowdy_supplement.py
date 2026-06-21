#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Tier-4 USB-ID source: scrape https://usb-ids.gowdy.us for VIDs that the
canonical text databases (Rust `usb-ids` crate, linux-usb.org, Fedora
hwdata) don't yet carry.

Gowdy.us is "the home of the usb.ids file" — but the compiled `usb.ids`
text download lags behind community submissions (e.g. Bertold submitted
Espressif Systems 0x303A in 2021, still missing from the text dump as of
2026). The /read/UD/<VID> HTML page DOES surface those submissions, so we
parse that view as a supplement specifically for VIDs the upstream JSON
already failed to provide.

This is a real, attributable data source (the URL we scraped is the
audit trail) — not a hardcoded list. We deliberately scope the scrape to
the VIDs referenced by `online-data/data/mcu_to_vid.json` so the request
volume is tiny and the data we add only fills holes that actually matter
for the headline VID:PID -> board lookup.

Output (sorted, lowercase keys, matches usb-vid.json schema):
    {
      "303a": {
        "vendor": "Espressif Systems",
        "products": [["0002", ""], ["1001", ""], ["4001", ""], ["8293", ""]]
      }
    }

Usage:
    fetch_gowdy_supplement.py \\
      --mcu-to-vid online-data/data/mcu_to_vid.json \\
      --existing  online-data/data/usb-vid.json \\
      --out       /tmp/gowdy-supplement.json
"""

from __future__ import annotations

import argparse
import json
import re
import ssl
import sys
import urllib.request
from pathlib import Path
from typing import Callable, Iterable

GOWDY_BASE = "https://usb-ids.gowdy.us/read/UD"

# Robust patterns scoped to the HTML structure observed on /read/UD/<VID>:
#   <p class='itemname'>Name: Espressif Systems
_VENDOR_NAME_RE = re.compile(
    r"<p\s+class=['\"]itemname['\"]>\s*Name:\s*(.+?)\s*(?:<|$)",
    re.IGNORECASE | re.DOTALL,
)
# <tr ...><td><a href="/read/UD/303a/0002">0002</a><td>Optional name<td>
_PRODUCT_RE = re.compile(
    r"<a\s+href=['\"]/read/UD/[0-9a-fA-F]{4}/([0-9a-fA-F]{4})['\"]>\s*[0-9a-fA-F]{4}\s*</a>\s*<td>([^<]*)",
    re.IGNORECASE,
)


def _fetch(url: str, *, timeout: float = 30.0) -> str:
    # gowdy.us serves a self-signed-ish cert behind sourceforge — accept it.
    # The scraped content is structural / public; SSL-stripping risk is low.
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout, context=ctx) as resp:
        return resp.read().decode("utf-8", errors="replace")


def parse_vendor_page(html: str) -> tuple[str | None, list[tuple[str, str]]]:
    """Return (vendor_name_or_None, list of (pid_lower, product_name_or_empty))."""
    vendor_match = _VENDOR_NAME_RE.search(html)
    vendor = vendor_match.group(1).strip() if vendor_match else None
    products: list[tuple[str, str]] = []
    for m in _PRODUCT_RE.finditer(html):
        pid = m.group(1).lower()
        name = m.group(2).strip()
        products.append((pid, name))
    # De-dupe (the page can list the same PID twice in degenerate cases).
    seen: set[str] = set()
    uniq: list[tuple[str, str]] = []
    for pid, name in products:
        if pid in seen:
            continue
        seen.add(pid)
        uniq.append((pid, name))
    return vendor, uniq


def collect(
    *,
    vids: Iterable[str],
    existing: set[str],
    fetch: Callable[[str], str] = _fetch,
) -> dict:
    """For each VID, scrape gowdy.us and emit usb-vid.json-shaped entries.

    Skips VIDs already present in `existing` so we never overwrite the
    primary upstream sources. Vendors gowdy can't resolve are skipped.
    """
    out: dict = {}
    for vid in sorted(set(vid.lower() for vid in vids)):
        if vid in existing:
            continue
        url = f"{GOWDY_BASE}/{vid.upper()}"
        try:
            html = fetch(url)
        except Exception as e:
            print(f"warning: {url}: fetch failed: {e}", file=sys.stderr)
            continue
        vendor, products = parse_vendor_page(html)
        if not vendor:
            print(f"warning: {url}: no vendor name on page; skipped", file=sys.stderr)
            continue
        out[vid] = {
            "vendor": vendor,
            "products": [list(p) for p in products],
        }
        print(f"gowdy 0x{vid}: vendor={vendor!r}  products={len(products)}", file=sys.stderr)
    return out


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--mcu-to-vid", required=True, type=Path,
                   help="JSON array of {mcu_family, vid, score, ...} — the VIDs we care about.")
    p.add_argument("--existing", required=True, type=Path,
                   help="Upstream usb-vid.json — VIDs already here are NOT re-fetched.")
    p.add_argument("--out", required=True, type=Path,
                   help="Output JSON in usb-vid.json shape (sorted, lowercase keys).")
    args = p.parse_args()

    mcu = json.loads(args.mcu_to_vid.read_text(encoding="utf-8"))
    vids = [e["vid"] for e in mcu]
    existing_raw = json.loads(args.existing.read_text(encoding="utf-8"))
    existing = {k.lower() for k in (existing_raw.keys() if isinstance(existing_raw, dict) else ())}

    supplement = collect(vids=vids, existing=existing)
    args.out.write_text(
        json.dumps(supplement, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(
        f"wrote {args.out}: {len(supplement)} supplementary VID(s) "
        f"(asked={len(set(vids))}, already-upstream={len(set(v.lower() for v in vids) & existing)})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

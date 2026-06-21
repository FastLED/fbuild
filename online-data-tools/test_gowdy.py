#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_gowdy_supplement + overlay_usb_vid.

The HTML fixture in `parse_vendor_page` tests is captured verbatim from
https://usb-ids.gowdy.us/read/UD/303A (the 0x303A Espressif page) so the
parser regression is grounded in real production HTML, not a synthetic
sample. No network in the test suite — `fetch` is stubbed via a callable.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_gowdy_supplement   # noqa: E402
import overlay_usb_vid          # noqa: E402


GOWDY_303A_HTML = """\
<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01//EN" "http://www.w3.org/TR/html4/strict.dtd">
<html lang="en"><head><title>USB Devices</title></head><body>
<div class='discussion'>
<h2>Discussion</h2><div class='unseen-history'>
<p class='itemname'>Name: Espressif Systems
<p class='author'>Bertold
<p class='time'>2021-11-10 14:17:35
</div>
<p><a href='?action=newhistory'>Discuss</a>
</div>
<h2>Devices</h2>
<table class="subnodes">
<tr class="unnamedItem"><td><a href="/read/UD/303a/0002">0002</a><td><td>
<tr class="unnamedItem"><td><a href="/read/UD/303a/1001">1001</a><td>USB JTAG/serial debug unit<td>
<tr class="unnamedItem"><td><a href="/read/UD/303a/4001">4001</a><td><td>
<tr class="unnamedItem"><td><a href="/read/UD/303a/8293">8293</a><td><td>
</table>
</body></html>
"""

GOWDY_BARE_PAGE = """\
<html><body><p>No such vendor</p></body></html>
"""


# --------------------------------------------------------------------------- #
# parse_vendor_page
# --------------------------------------------------------------------------- #

def test_parse_vendor_extracts_name_and_pids() -> None:
    vendor, products = fetch_gowdy_supplement.parse_vendor_page(GOWDY_303A_HTML)
    assert vendor == "Espressif Systems"
    pids = [p for p, _ in products]
    assert pids == ["0002", "1001", "4001", "8293"]
    # Product names are captured when present.
    names = {p: n for p, n in products}
    assert names["1001"] == "USB JTAG/serial debug unit"
    assert names["0002"] == ""


def test_parse_vendor_page_with_no_vendor_returns_none() -> None:
    vendor, products = fetch_gowdy_supplement.parse_vendor_page(GOWDY_BARE_PAGE)
    assert vendor is None
    assert products == []


def test_parse_vendor_de_dupes_pids() -> None:
    html = GOWDY_303A_HTML.replace(
        '<a href="/read/UD/303a/4001">4001</a>',
        '<a href="/read/UD/303a/4001">4001</a></tr><tr><td><a href="/read/UD/303a/4001">4001</a>',
    )
    _vendor, products = fetch_gowdy_supplement.parse_vendor_page(html)
    pids = [p for p, _ in products]
    assert pids.count("4001") == 1


# --------------------------------------------------------------------------- #
# collect — wire parse + the existing-VID skip-set + the injected fetch
# --------------------------------------------------------------------------- #

def test_collect_skips_existing_vids() -> None:
    captured: list[str] = []

    def fake_fetch(url: str) -> str:
        captured.append(url)
        return GOWDY_303A_HTML

    out = fetch_gowdy_supplement.collect(
        vids=["303a", "10c4"],
        existing={"10c4"},  # already in upstream — skip
        fetch=fake_fetch,
    )
    assert list(out.keys()) == ["303a"]
    assert out["303a"]["vendor"] == "Espressif Systems"
    assert len(out["303a"]["products"]) == 4
    assert captured == ["https://usb-ids.gowdy.us/read/UD/303A"]


def test_collect_swallows_per_vid_failures() -> None:
    def flaky_fetch(url: str) -> str:
        if "BAD0" in url:
            raise RuntimeError("simulated network error")
        return GOWDY_303A_HTML

    out = fetch_gowdy_supplement.collect(
        vids=["303a", "BAD0"], existing=set(), fetch=flaky_fetch,
    )
    assert list(out.keys()) == ["303a"], "good VID must still land despite a sibling failure"


def test_collect_skips_vendors_with_no_name_on_page() -> None:
    def fetch(url: str) -> str:
        return GOWDY_BARE_PAGE

    out = fetch_gowdy_supplement.collect(
        vids=["beef"], existing=set(), fetch=fetch,
    )
    assert out == {}


# --------------------------------------------------------------------------- #
# overlay_usb_vid
# --------------------------------------------------------------------------- #

def test_overlay_gap_fill_adds_missing_vids_only() -> None:
    upstream = {
        "10c4": {"vendor": "Silicon Labs", "products": [["ea60", "CP210x"]]},
    }
    supplement = {
        "303a": {"vendor": "Espressif Systems", "products": [["4002", ""]]},
        "10c4": {"vendor": "WRONG NAME", "products": [["dead", "bad"]]},  # must NOT win
    }
    merged, changed = overlay_usb_vid.overlay(upstream, supplement, mode="gap-fill")
    assert changed == 1
    # Upstream wins for 10c4 — no merging of name OR products.
    assert merged["10c4"]["vendor"] == "Silicon Labs"
    assert merged["10c4"]["products"] == [["ea60", "CP210x"]]
    # 303a is freshly added.
    assert merged["303a"]["vendor"] == "Espressif Systems"
    # Sorted by VID.
    assert list(merged.keys()) == ["10c4", "303a"]


def test_overlay_vendor_override_replaces_name_keeps_products() -> None:
    """In vendor-override mode the supplement is the higher-authority source
    for the vendor NAME but never disturbs the upstream products list."""
    upstream = {
        "10c4": {
            "vendor": "Silicon Labs",
            "products": [["ea60", "CP210x"], ["ea71", "CP2102N"]],
        },
        "0403": {"vendor": "Future Technology Devices", "products": [["6001", "FT232"]]},
    }
    supplement = {
        "10c4": {"vendor": "Silicon Laboratories Inc.", "products": []},
        "303a": {"vendor": "Espressif Systems",         "products": []},
    }
    merged, changed = overlay_usb_vid.overlay(
        upstream, supplement, mode="vendor-override",
    )
    # 10c4 renamed; 303a added → 2 changes.
    assert changed == 2
    # Renamed vendor; products preserved verbatim.
    assert merged["10c4"]["vendor"] == "Silicon Laboratories Inc."
    assert merged["10c4"]["products"] == [["ea60", "CP210x"], ["ea71", "CP2102N"]]
    # Untouched upstream entry stays as-is.
    assert merged["0403"]["vendor"] == "Future Technology Devices"
    # New entry added (products empty because supplement is vendor-only).
    assert merged["303a"]["vendor"] == "Espressif Systems"
    assert merged["303a"]["products"] == []


def test_overlay_vendor_override_skips_when_name_unchanged() -> None:
    """If the supplement repeats the upstream name verbatim, no change."""
    upstream    = {"10c4": {"vendor": "Silicon Labs", "products": []}}
    supplement  = {"10c4": {"vendor": "Silicon Labs", "products": []}}
    _merged, changed = overlay_usb_vid.overlay(
        upstream, supplement, mode="vendor-override",
    )
    assert changed == 0


def test_overlay_invalid_mode_rejected() -> None:
    with pytest.raises(ValueError):
        overlay_usb_vid.overlay({}, {}, mode="bogus")


def test_overlay_does_not_mutate_input() -> None:
    upstream = {"10c4": {"vendor": "Silicon Labs", "products": []}}
    overlay_usb_vid.overlay(upstream, {"303a": {"vendor": "X", "products": []}})
    assert "303a" not in upstream
    # vendor-override case
    upstream2 = {"10c4": {"vendor": "Silicon Labs", "products": []}}
    overlay_usb_vid.overlay(
        upstream2, {"10c4": {"vendor": "Other", "products": []}},
        mode="vendor-override",
    )
    assert upstream2["10c4"]["vendor"] == "Silicon Labs"


def test_overlay_main_emits_file(tmp_path: Path) -> None:
    up = tmp_path / "upstream.json"
    sup = tmp_path / "sup.json"
    out = tmp_path / "out.json"
    up.write_text(json.dumps({"10c4": {"vendor": "Silicon Labs", "products": []}}),
                  encoding="utf-8")
    sup.write_text(json.dumps({"303a": {"vendor": "Espressif Systems",
                                         "products": [["4002", ""]]}}),
                   encoding="utf-8")
    sys.argv = [
        "overlay_usb_vid.py",
        "--upstream", str(up),
        "--supplement", str(sup),
        "--out", str(out),
    ]
    rc = overlay_usb_vid.main()
    assert rc == 0
    data = json.loads(out.read_text(encoding="utf-8"))
    assert "303a" in data and data["303a"]["vendor"] == "Espressif Systems"


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))

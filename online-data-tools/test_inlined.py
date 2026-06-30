#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for vendor_names_inlined.py — the canonical curated overlay.

Locks in the known-critical VID -> vendor name pairs that motivated
introducing the overlay in the first place (issue #718): 0x303A
(Espressif), 0x2E8A (Raspberry Pi Foundation), plus several other
common-MCU VIDs referenced by mcu_to_vid.json. If anyone removes one
of these from the inlined dict the headline VID:PID -> board query on
the www page silently breaks, so the regression here is intentional.

Also asserts shape invariants: keys are 4-hex-digit lowercase, values
are non-empty strings, no entry duplicates, no HTML entities or NBSP
characters slipped past the curation pipeline.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import vendor_names_inlined  # noqa: E402


_HEX4_LOWER = re.compile(r"^[0-9a-f]{4}$")


# --------------------------------------------------------------------------- #
# Critical-VID locks (will fail loudly if a curator drops them).
# --------------------------------------------------------------------------- #

@pytest.mark.parametrize("vid, expected_substring", [
    ("303a", "Espressif"),
    ("2e8a", "Raspberry Pi"),
    ("045b", "Renesas"),
    ("2544", "Silicon Labs"),
    ("1b4f", "SparkFun"),
    ("2914", "Kent"),                  # Kent Displays
    ("ffff", "Wrong vendor ID"),       # all-bits-set sentinel
])
def test_critical_vids_present(vid: str, expected_substring: str) -> None:
    assert vid in vendor_names_inlined.VENDOR_NAMES, (
        f"critical VID 0x{vid} dropped from inlined overlay; restore from "
        f"ids4.json"
    )
    name = vendor_names_inlined.VENDOR_NAMES[vid]
    assert expected_substring.lower() in name.lower(), (
        f"0x{vid} now maps to {name!r}, expected substring {expected_substring!r}"
    )


# --------------------------------------------------------------------------- #
# Shape invariants
# --------------------------------------------------------------------------- #

def test_all_keys_are_4_hex_lower() -> None:
    bad = [k for k in vendor_names_inlined.VENDOR_NAMES if not _HEX4_LOWER.match(k)]
    assert not bad, f"keys must be 4-hex-digit lowercase; bad: {bad[:10]}"


def test_all_values_are_non_empty_strings() -> None:
    bad = {
        k: v for k, v in vendor_names_inlined.VENDOR_NAMES.items()
        if not (isinstance(v, str) and v.strip())
    }
    assert not bad, f"values must be non-empty strings; bad: {list(bad.items())[:5]}"


def test_no_html_entities_or_nbsp_survive() -> None:
    """The curation pipeline html.unescape()s and NFKCs all values; if any
    entity-encoded or NBSP-containing strings re-appear, we regressed."""
    entity_re = re.compile(r"&(amp|lt|gt|quot|apos|#\d+|#x[0-9a-fA-F]+);")
    bad = []
    for k, v in vendor_names_inlined.VENDOR_NAMES.items():
        if "\xa0" in v or entity_re.search(v):
            bad.append((k, v))
    assert not bad, f"raw HTML entities / NBSP survived curation: {bad[:5]}"


def test_no_duplicate_keys_or_blank_entries() -> None:
    keys = list(vendor_names_inlined.VENDOR_NAMES.keys())
    assert len(set(keys)) == len(keys), "duplicate keys (post-dict — impossible?)"
    # Round-trip via JSON to catch any non-serializable garbage that
    # snuck in via copy-paste.
    blob = json.dumps(vendor_names_inlined.VENDOR_NAMES, ensure_ascii=False)
    rt = json.loads(blob)
    assert rt == vendor_names_inlined.VENDOR_NAMES


# --------------------------------------------------------------------------- #
# as_supplement() — the overlay-compatible export
# --------------------------------------------------------------------------- #

def test_as_supplement_shape() -> None:
    sup = vendor_names_inlined.as_supplement()
    assert isinstance(sup, dict)
    assert len(sup) == len(vendor_names_inlined.VENDOR_NAMES)
    # Spot-check the 303a entry shape matches usb-vid.json.
    e = sup["303a"]
    assert e == {"vendor": "Espressif Systems", "products": []}


def test_as_supplement_main_writes_overlay_file(tmp_path: Path) -> None:
    out = tmp_path / "inlined.json"
    sys.argv = ["vendor_names_inlined.py", "--out", str(out)]
    rc = vendor_names_inlined.main()
    assert rc == 0
    data = json.loads(out.read_text(encoding="utf-8"))
    assert "303a" in data and data["303a"]["vendor"] == "Espressif Systems"
    assert data["303a"]["products"] == []


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))

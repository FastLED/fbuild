#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_teensy_usb_pids.py.

Fixtures are small, network-free snippets from PJRC Teensy core headers and
`teensy_loader_cli.c`.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_teensy_usb_pids  # noqa: E402


USB_DESC_FIXTURE = """\
#if defined(USB_SERIAL)
  #define VENDOR_ID 0x16C0
  #define PRODUCT_ID 0x0483
  #define PRODUCT_NAME {'U','S','B',' ','S','e','r','i','a','l'}
#elif defined(USB_DUAL_SERIAL)
  #define VENDOR_ID 0x16C0
  #define PRODUCT_ID 0x048B
  #define PRODUCT_NAME {'D','u','a','l',' ','S','e','r','i','a','l'}
#elif defined(USB_MIDI_SERIAL)
  #define VENDOR_ID 0x16C0
  #define PRODUCT_ID 0x0489
  #define PRODUCT_NAME {'T','e','e','n','s','y',' ','M','I','D','I'}
#elif defined(USB_MIDI16_SERIAL)
  #define VENDOR_ID 0x16C0
  #define PRODUCT_ID 0x0489
  #define PRODUCT_NAME {'T','e','e','n','s','y',' ','M','I','D','I','x','1','6'}
#elif defined(USB_MTPDISK_SERIAL)
  #define VENDOR_ID 0x16C0
  #define PRODUCT_ID 0x04D5
  #define PRODUCT_NAME {'T','e','e','n','s','y',' ','M','T','P',' ','D','i','s','k'}
#elif defined(USB_OTHER_VENDOR)
  #define VENDOR_ID 0x9999
  #define PRODUCT_ID 0x1234
  #define PRODUCT_NAME {'N','o','i','s','e'}
#endif
"""

LOADER_FIXTURE = """\
rebootor = open_usb_device(0x16C0, 0x0477);
libusb_teensy_handle = open_usb_device(0x16C0, 0x0478);
"""


def test_parse_product_name_char_array() -> None:
    assert (
        fetch_teensy_usb_pids.parse_product_name(
            "#define PRODUCT_NAME {'T','e','e','n','s','y'}"
        )
        == "Teensy"
    )


def test_parse_usb_desc_extracts_teensy_products_only() -> None:
    parsed = fetch_teensy_usb_pids.parse_usb_desc(USB_DESC_FIXTURE)
    assert parsed == {
        "0483": {"USB Serial"},
        "0489": {"Teensy MIDI", "Teensy MIDIx16"},
        "048b": {"Dual Serial"},
        "04d5": {"Teensy MTP Disk"},
    }


def test_collapse_products_handles_known_duplicate_pids() -> None:
    parsed = fetch_teensy_usb_pids.parse_usb_desc(USB_DESC_FIXTURE)
    collapsed = fetch_teensy_usb_pids.collapse_products(parsed)
    assert collapsed["0489"] == "Teensyduino MIDI + Serial"
    assert collapsed["04d5"] == "Teensyduino MTP Disk + Serial"
    assert collapsed["0483"] == "Teensyduino Serial"
    assert collapsed["048b"] == "Teensyduino Dual Serial"


def test_collapse_products_rejects_unknown_duplicate_pid() -> None:
    with pytest.raises(ValueError, match="unhandled names"):
        fetch_teensy_usb_pids.collapse_products({"1234": {"One", "Two"}})


def test_parse_loader_products() -> None:
    assert fetch_teensy_usb_pids.parse_loader_products(LOADER_FIXTURE) == {
        "0477": "Teensy Rebootor",
        "0478": "Teensy HalfKay Bootloader",
    }


def test_collect_merges_usb_desc_and_loader_rows() -> None:
    fixtures = {
        "usb": USB_DESC_FIXTURE,
        "loader": LOADER_FIXTURE,
    }

    def fake_fetch(url: str) -> str:
        return fixtures[url]

    out = fetch_teensy_usb_pids.collect(
        fetch=fake_fetch,
        usb_desc_urls=("usb",),
        loader_url="loader",
    )
    assert out["16c0:0478"] == {
        "vendor": "Van Ooijen Technische Informatica",
        "product": "Teensy HalfKay Bootloader",
    }
    assert out["16c0:0489"]["product"] == "Teensyduino MIDI + Serial"
    assert out["16c0:04d5"]["product"] == "Teensyduino MTP Disk + Serial"
    assert "16c0:1234" not in out
    assert list(out) == sorted(out)


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "teensy.json"
    old_collect = fetch_teensy_usb_pids.collect
    try:
        fetch_teensy_usb_pids.collect = lambda: {
            "16c0:04d5": {
                "vendor": "Van Ooijen Technische Informatica",
                "product": "Teensyduino MTP Disk + Serial",
            }
        }
        sys.argv = ["fetch_teensy_usb_pids.py", "--out", str(out)]
        assert fetch_teensy_usb_pids.main() == 0
    finally:
        fetch_teensy_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "16c0:04d5": {
            "vendor": "Van Ooijen Technische Informatica",
            "product": "Teensyduino MTP Disk + Serial",
        }
    }

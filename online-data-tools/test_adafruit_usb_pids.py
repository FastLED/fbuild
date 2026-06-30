#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_adafruit_usb_pids.py."""

from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_adafruit_usb_pids  # noqa: E402


BOARDS_FIXTURE = """\
adafruit_feather_m0_express.name=Adafruit Feather M0 Express (SAMD21)
adafruit_feather_m0_express.vid.0=0x239A
adafruit_feather_m0_express.pid.0=0x801B
adafruit_feather_m0_express.vid.1=0x239A
adafruit_feather_m0_express.pid.1=0x001B
arduino_zero.name=Arduino Zero
arduino_zero.vid.0=0x2341
arduino_zero.pid.0=0x004D
"""

TINYUF2_FIXTURE = """\
#define USB_VID                  0x239A
#define USB_PID                  0x011B
#define USB_MANUFACTURER         "Adafruit"
#define USB_PRODUCT              "Feather ESP32-S3"
"""

CIRCUITPYTHON_FIXTURE = """\
USB_VID = 0x239A
USB_PID = 0x8106
USB_PRODUCT = "KB2040"
USB_MANUFACTURER = "Adafruit"
"""


def test_parse_boards_txt_extracts_adafruit_vid_only() -> None:
    rows = fetch_adafruit_usb_pids.parse_boards_txt(BOARDS_FIXTURE)

    assert rows["239a:801b"] == {
        "vendor": "Adafruit",
        "product": "Adafruit Feather M0 Express (SAMD21)",
    }
    assert rows["239a:001b"]["product"] == "Adafruit Feather M0 Express (SAMD21)"
    assert "2341:004d" not in rows


def test_parse_tinyuf2_descriptor_prefixes_manufacturer() -> None:
    rows = fetch_adafruit_usb_pids.parse_usb_descriptor_text(
        TINYUF2_FIXTURE,
        syntax="c",
    )

    assert rows == {
        "239a:011b": {
            "vendor": "Adafruit",
            "product": "Adafruit Feather ESP32-S3",
        }
    }


def test_parse_circuitpython_descriptor_prefixes_manufacturer() -> None:
    rows = fetch_adafruit_usb_pids.parse_usb_descriptor_text(
        CIRCUITPYTHON_FIXTURE,
        syntax="make",
    )

    assert rows == {
        "239a:8106": {
            "vendor": "Adafruit",
            "product": "Adafruit KB2040",
        }
    }


def test_collect_merges_sources_as_fill_gaps() -> None:
    adafruit_source = fetch_adafruit_usb_pids.BoardSource("boards", "boards")

    def fake_fetch(url: str) -> str:
        if url == "boards":
            return BOARDS_FIXTURE
        if url.endswith("tiny/board.h"):
            return TINYUF2_FIXTURE
        if url.endswith("cp/mpconfigboard.mk"):
            return CIRCUITPYTHON_FIXTURE
        raise AssertionError(url)

    def fake_json(url: str) -> dict:
        if url == "tiny-tree":
            return {"tree": [{"path": "ports/espressif/boards/adafruit_tiny/board.h"}]}
        if url == "cp-tree":
            return {"tree": [{"path": "ports/raspberrypi/boards/adafruit_cp/mpconfigboard.mk"}]}
        raise AssertionError(url)

    rows = fetch_adafruit_usb_pids.collect_arduino_boards(
        fetch_text=fake_fetch,
        sources=(adafruit_source,),
    )
    rows = fetch_adafruit_usb_pids._merge_fill_gaps(
        rows,
        fetch_adafruit_usb_pids.collect_tinyuf2(
            fetch_text=fake_fetch,
            fetch_json=fake_json,
            tree_url="tiny-tree",
            raw_base="https://raw/tiny",
        ),
    )
    rows = fetch_adafruit_usb_pids._merge_fill_gaps(
        rows,
        fetch_adafruit_usb_pids.collect_circuitpython(
            fetch_text=fake_fetch,
            fetch_json=fake_json,
            tree_url="cp-tree",
            raw_base="https://raw/cp",
        ),
    )

    assert rows["239a:801b"]["product"] == "Adafruit Feather M0 Express (SAMD21)"
    assert rows["239a:011b"]["product"] == "Adafruit Feather ESP32-S3"
    assert rows["239a:8106"]["product"] == "Adafruit KB2040"


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "adafruit.json"
    old_collect = fetch_adafruit_usb_pids.collect
    try:
        fetch_adafruit_usb_pids.collect = lambda: {
            "239a:801b": {
                "vendor": "Adafruit",
                "product": "Adafruit Feather M0 Express (SAMD21)",
            }
        }
        sys.argv = ["fetch_adafruit_usb_pids.py", "--out", str(out)]
        assert fetch_adafruit_usb_pids.main() == 0
    finally:
        fetch_adafruit_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "239a:801b": {
            "vendor": "Adafruit",
            "product": "Adafruit Feather M0 Express (SAMD21)",
        }
    }

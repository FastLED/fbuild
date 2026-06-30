#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_sparkfun_usb_pids.py."""

from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_sparkfun_usb_pids  # noqa: E402


PROMICRO_FIXTURE = """\
promicro.name=SparkFun Pro Micro
promicro.build.vid=0x1b4f
promicro.menu.cpu.8MHzatmega32U4=ATmega32U4 (3.3V, 8 MHz)
promicro.menu.cpu.8MHzatmega32U4.build.pid.0=0x9203
promicro.menu.cpu.8MHzatmega32U4.build.pid.1=0x9204
promicro.menu.cpu.8MHzatmega32U4.build.pid=0x9204
promicro.menu.cpu.16MHzatmega32U4=ATmega32U4 (5V, 16 MHz)
promicro.menu.cpu.16MHzatmega32U4.build.pid.0=0x9205
promicro.menu.cpu.16MHzatmega32U4.build.pid.1=0x9206
promicro.menu.cpu.16MHzatmega32U4.build.pid=0x9206
"""

SAMD_FIXTURE = """\
samd21_dev.name=SparkFun SAMD21 Dev Breakout
samd21_dev.vid.0=0x1B4F
samd21_dev.pid.0=0x8D21
samd21_dev.vid.1=0x1B4F
samd21_dev.pid.1=0x0D21
samd21_mini.name=SparkFun SAMD21 Mini Breakout
samd21_mini.vid.0=0x1B4F
samd21_mini.pid.0=0x8D21
arduino_zero.name=Arduino Zero
arduino_zero.vid.0=0x2341
arduino_zero.pid.0=0x004D
"""

DUPLICATE_INDEX_FIXTURE = """\
sparkfunnrf52840mini.name=SparkFun Pro nRF52840 Mini
sparkfunnrf52840mini.vid.2=0x1B4F
sparkfunnrf52840mini.pid.2=0x8029
sparkfunnrf52840mini.vid.2=0x1B4F
sparkfunnrf52840mini.pid.2=0x802A
"""

UF2_FIXTURE = """\
#define PRODUCT_NAME "SparkFun SAMD51 Thing+"
#define USB_VID 0x1B4F
#define USB_PID 0x0016
"""

CP_FIXTURE = """\
USB_VID = 0x1B4F
USB_PID = 0x8D24
USB_PRODUCT = "SparkFun Qwiic Micro"
USB_MANUFACTURER = "SparkFun Electronics"
"""

PLATFORMIO_FIXTURE = {
    "build": {"hwids": [["0x1B4F", "0x0027"], ["0x303A", "0x1001"]]},
    "name": "SparkFun ESP32-S2 Thing Plus",
    "vendor": "SparkFun",
}


def test_parse_boards_txt_inherits_menu_pid_vid() -> None:
    rows = fetch_sparkfun_usb_pids.parse_boards_txt(PROMICRO_FIXTURE)

    assert rows["1b4f:9203"]["product"] == (
        "SparkFun Pro Micro ATmega32U4 (3.3V, 8 MHz)"
    )
    assert rows["1b4f:9206"]["product"] == (
        "SparkFun Pro Micro ATmega32U4 (5V, 16 MHz)"
    )


def test_parse_boards_txt_collapses_duplicate_products_and_filters_vid() -> None:
    rows = fetch_sparkfun_usb_pids.parse_boards_txt(SAMD_FIXTURE)

    assert rows["1b4f:8d21"]["product"] == (
        "SparkFun SAMD21 Dev Breakout / SparkFun SAMD21 Mini Breakout"
    )
    assert rows["1b4f:0d21"]["product"] == "SparkFun SAMD21 Dev Breakout"
    assert "2341:004d" not in rows


def test_parse_boards_txt_preserves_repeated_index_pids() -> None:
    rows = fetch_sparkfun_usb_pids.parse_boards_txt(DUPLICATE_INDEX_FIXTURE)

    assert rows["1b4f:8029"]["product"] == "SparkFun Pro nRF52840 Mini"
    assert rows["1b4f:802a"]["product"] == "SparkFun Pro nRF52840 Mini"


def test_parse_usb_descriptors() -> None:
    assert fetch_sparkfun_usb_pids.parse_usb_descriptor_text(
        UF2_FIXTURE,
        syntax="c",
    ) == {
        "1b4f:0016": {
            "vendor": "SparkFun",
            "product": "SparkFun SAMD51 Thing+",
        }
    }

    assert fetch_sparkfun_usb_pids.parse_usb_descriptor_text(
        CP_FIXTURE,
        syntax="make",
    ) == {
        "1b4f:8d24": {
            "vendor": "SparkFun",
            "product": "SparkFun Qwiic Micro",
        }
    }


def test_parse_platformio_hwids_filters_to_sparkfun_vid() -> None:
    rows = fetch_sparkfun_usb_pids.parse_platformio_board_json(
        json.dumps(PLATFORMIO_FIXTURE),
    )

    assert rows == {
        "1b4f:0027": {
            "vendor": "SparkFun",
            "product": "SparkFun ESP32-S2 Thing Plus",
        }
    }


def test_collect_all_keeps_first_party_name_over_supplemental() -> None:
    def fake_text(url: str) -> str:
        if url == "boards":
            return "thing.name=SparkFun First Party\nthing.vid.0=0x1B4F\nthing.pid.0=0x0027\n"
        if url.endswith("sparkfun_weak/mpconfigboard.mk"):
            return (
                "USB_VID = 0x1B4F\n"
                "USB_PID = 0x0027\n"
                'USB_PRODUCT = "SparkFun Weak Name"\n'
                'USB_MANUFACTURER = "SparkFun Electronics"\n'
            )
        raise AssertionError(url)

    def fake_json(url: str) -> dict:
        if url == "cp-tree":
            return {
                "tree": [
                    {
                        "path": "ports/atmel-samd/boards/"
                        "sparkfun_weak/mpconfigboard.mk"
                    }
                ]
            }
        raise AssertionError(url)

    first_party = fetch_sparkfun_usb_pids.collect_first_party(
        fetch_text=fake_text,
        board_sources=(fetch_sparkfun_usb_pids.TextSource("boards", "boards"),),
        descriptor_sources=(),
    )
    weak = fetch_sparkfun_usb_pids.collect_circuitpython(
        fetch_text=fake_text,
        fetch_json=fake_json,
        tree_url="cp-tree",
        raw_base="https://raw",
    )

    assert fetch_sparkfun_usb_pids._merge_fill_gaps(first_party, weak) == {
        "1b4f:0027": {
            "vendor": "SparkFun",
            "product": "SparkFun First Party",
        }
    }


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "sparkfun.json"
    old_collect = fetch_sparkfun_usb_pids.collect
    try:
        fetch_sparkfun_usb_pids.collect = lambda tier="all": {
            "1b4f:9206": {
                "vendor": "SparkFun",
                "product": "SparkFun Pro Micro ATmega32U4 (5V, 16 MHz)",
            }
        }
        sys.argv = ["fetch_sparkfun_usb_pids.py", "--out", str(out)]
        assert fetch_sparkfun_usb_pids.main() == 0
    finally:
        fetch_sparkfun_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "1b4f:9206": {
            "vendor": "SparkFun",
            "product": "SparkFun Pro Micro ATmega32U4 (5V, 16 MHz)",
        }
    }

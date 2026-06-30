#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_espressif_usb_pids.py.

Fixtures are small, network-free snippets from the official
`espressif/usb-pids` text format.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_espressif_usb_pids  # noqa: E402


CUSTOMER_FIXTURE = """\
Allocated PIDs under Espressifs VID (0x303a).

PID | Product name
0x8001 | Unexpected Maker TinyS2 - Arduino
0x8002 | Unexpected Maker TinyS2 - CircuitPython
0x800A | ATMegaZero ESP32-S2 - Arduino
0x9000 | Unallocated
not a row
"""

DEVBOARD_FIXTURE = """\
PID | Product name
0x7002 | ESP32-S3-DevKitC-1 - UF2 Bootloader
0x7012 | ESP32-P4-Function-EV - UF2 Bootloader
"""


def test_parse_pid_registry_normalizes_and_skips_noise() -> None:
    parsed = fetch_espressif_usb_pids.parse_pid_registry(CUSTOMER_FIXTURE)
    assert parsed == {
        "8001": "Unexpected Maker TinyS2 - Arduino",
        "8002": "Unexpected Maker TinyS2 - CircuitPython",
        "800a": "ATMegaZero ESP32-S2 - Arduino",
    }


def test_parse_pid_registry_rejects_conflicting_duplicate() -> None:
    text = "0x8001 | First\n0x8001 | Second\n"
    with pytest.raises(ValueError, match="duplicate Espressif PID"):
        fetch_espressif_usb_pids.parse_pid_registry(text)


def test_collect_merges_builtins_devboards_and_customer_rows() -> None:
    fixtures = {
        "devboards": DEVBOARD_FIXTURE,
        "customer": CUSTOMER_FIXTURE,
    }

    def fake_fetch(url: str) -> str:
        return fixtures[url]

    out = fetch_espressif_usb_pids.collect(
        fetch=fake_fetch,
        urls=("devboards", "customer"),
    )
    assert out["303a:0002"]["product"] == "ESP32-S2 USB-OTG"
    assert out["303a:1001"]["product"] == "USB JTAG/serial debug unit"
    assert out["303a:4001"]["product"] == "ESP-IDF TinyUSB serial device"
    assert out["303a:7002"]["product"] == "ESP32-S3-DevKitC-1 - UF2 Bootloader"
    assert out["303a:7012"]["product"] == "ESP32-P4-Function-EV - UF2 Bootloader"
    assert out["303a:8001"]["product"] == "Unexpected Maker TinyS2 - Arduino"
    assert out["303a:8001"]["vendor"] == "Espressif Systems"
    assert list(out) == sorted(out)


def test_collect_keeps_builtin_name_when_registry_repeats_pid() -> None:
    def fake_fetch(_url: str) -> str:
        return "0x1001 | Different registry name\n"

    out = fetch_espressif_usb_pids.collect(fetch=fake_fetch, urls=("registry",))
    assert out["303a:1001"]["product"] == "USB JTAG/serial debug unit"


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "espressif.json"
    old_collect = fetch_espressif_usb_pids.collect
    try:
        fetch_espressif_usb_pids.collect = lambda: {
            "303a:8001": {
                "vendor": "Espressif Systems",
                "product": "Unexpected Maker TinyS2 - Arduino",
            }
        }
        sys.argv = ["fetch_espressif_usb_pids.py", "--out", str(out)]
        assert fetch_espressif_usb_pids.main() == 0
    finally:
        fetch_espressif_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "303a:8001": {
            "vendor": "Espressif Systems",
            "product": "Unexpected Maker TinyS2 - Arduino",
        }
    }

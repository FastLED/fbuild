#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_raspberrypi_usb_pids.py.

Fixtures are small, network-free snippets from Raspberry Pi's official
`raspberrypi/usb-pid` Markdown table format.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_raspberrypi_usb_pids  # noqa: E402


PID_TABLE_FIXTURE = """\
Vendor-ID = 0x2E8A

| Product ID | Company | Product Description | Product link |
| --- | --- | --- | --- |
| **Internal** | | | |
| 0x0003 | Raspberry Pi | Raspberry Pi RP2040 boot | [RP2040](https://www.raspberrypi.com/documentation/microcontrollers/raspberry-pi-pico.html) |
| 0x000C | Raspberry Pi | Raspberry Pi Debug Probe | [Debug Probe](https://www.raspberrypi.com/documentation/microcontrollers/debug-probe.html) |
| 0x0011 | Raspberry Pi | |
| **Commercial** ||||
| **0x1000 - 0x1fff** ||||
| 0x1004 | Reserved 2 |||
| 0x100E | Adafruit Industries LLC | Raspberry Pi Zero | https://circuitpython.org/board/raspberrypi_zero/ |
| 0x100f | Cytron Technologies | Cytron Maker Nano RP2040 | https://www.cytron.io/p-maker-nano-rp2040 |
"""


def test_parse_pid_table_normalizes_and_skips_noise() -> None:
    parsed = fetch_raspberrypi_usb_pids.parse_pid_table(PID_TABLE_FIXTURE)
    assert parsed == {
        "0003": "Raspberry Pi RP2040 boot",
        "000c": "Raspberry Pi Debug Probe",
        "100e": "Raspberry Pi Zero",
        "100f": "Cytron Maker Nano RP2040",
    }


def test_parse_pid_table_rejects_conflicting_duplicate() -> None:
    text = (
        "| Product ID | Company | Product Description | Product link |\n"
        "| --- | --- | --- | --- |\n"
        "| 0x0003 | Raspberry Pi | First | |\n"
        "| 0x0003 | Raspberry Pi | Second | |\n"
    )
    with pytest.raises(ValueError, match="duplicate Raspberry Pi PID"):
        fetch_raspberrypi_usb_pids.parse_pid_table(text)


def test_collect_uses_vid_owner_vendor_not_row_company() -> None:
    def fake_fetch(_url: str) -> str:
        return PID_TABLE_FIXTURE

    out = fetch_raspberrypi_usb_pids.collect(fetch=fake_fetch, url="registry")
    assert out["2e8a:0003"]["product"] == "Raspberry Pi RP2040 boot"
    assert out["2e8a:0003"]["vendor"] == "Raspberry Pi Foundation"
    assert out["2e8a:100e"]["product"] == "Raspberry Pi Zero"
    assert out["2e8a:100e"]["vendor"] == "Raspberry Pi Foundation"
    assert "2e8a:0011" not in out
    assert "2e8a:1004" not in out
    assert list(out) == sorted(out)


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "raspberrypi.json"
    old_collect = fetch_raspberrypi_usb_pids.collect
    try:
        fetch_raspberrypi_usb_pids.collect = lambda: {
            "2e8a:0003": {
                "vendor": "Raspberry Pi Foundation",
                "product": "Raspberry Pi RP2040 boot",
            }
        }
        sys.argv = ["fetch_raspberrypi_usb_pids.py", "--out", str(out)]
        assert fetch_raspberrypi_usb_pids.main() == 0
    finally:
        fetch_raspberrypi_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "2e8a:0003": {
            "vendor": "Raspberry Pi Foundation",
            "product": "Raspberry Pi RP2040 boot",
        }
    }

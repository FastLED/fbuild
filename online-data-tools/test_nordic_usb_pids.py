#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_nordic_usb_pids.py.

Fixtures are small, network-free snippets from Nordic's Programmer
`devices.ts` and `pc-nrf-dfu-js` README formats.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_nordic_usb_pids  # noqa: E402


PROGRAMMER_FIXTURE = """\
export enum VendorId {
    SEGGER = 0x1366,
    NORDIC_SEMICONDUCTOR = 0x1915,
}

export const USBProductIds = [0x521f, 0xc00a, 0xcafe];

export const McubootProductIds = [
    // Thingy91
    0x520f, 0x9100,
    // Thingy53
    0x530c,
    // nPM1300
    0x53ab,
    // nPM1300-Serial-Recovery
    0x53ac,
];

export const ModemProductIds = [
    // Thingy91
    0x520f, 0x9100,
];
"""

DFU_README_FIXTURE = """\
PCA10059 is a nRF52840 dongle.
The pre-programmed bootloader provides a USB device with vendor ID `0x1915`
and product ID `0x521f`.
"""


def test_parse_array_entries_uses_nearest_comment() -> None:
    entries = fetch_nordic_usb_pids.parse_array_entries(
        PROGRAMMER_FIXTURE, "McubootProductIds"
    )
    assert entries == [
        ("520f", "Thingy91"),
        ("9100", "Thingy91"),
        ("530c", "Thingy53"),
        ("53ab", "nPM1300"),
        ("53ac", "nPM1300-Serial-Recovery"),
    ]


def test_parse_programmer_devices_maps_official_pid_arrays() -> None:
    parsed = fetch_nordic_usb_pids.parse_programmer_devices(PROGRAMMER_FIXTURE)
    assert parsed == {
        "520f": "Nordic Thingy:91",
        "521f": "Nordic USB serial DFU",
        "530c": "Nordic Thingy:53",
        "53ab": "Nordic nPM1300",
        "53ac": "Nordic nPM1300 Serial Recovery",
        "9100": "Nordic Thingy:91",
        "c00a": "Nordic USB serial DFU",
        "cafe": "Nordic USB serial DFU",
    }


def test_parse_programmer_devices_rejects_wrong_vendor() -> None:
    with pytest.raises(ValueError, match="does not declare VID 0x1915"):
        fetch_nordic_usb_pids.parse_programmer_devices(
            PROGRAMMER_FIXTURE.replace("0x1915", "0x9999")
        )


def test_parse_dfu_readme_returns_specific_521f_override() -> None:
    assert fetch_nordic_usb_pids.parse_dfu_readme(DFU_README_FIXTURE) == {
        "521f": "PCA10059 nRF52840 Dongle USB SDFU bootloader"
    }


def test_collect_merges_programmer_rows_and_dfu_override() -> None:
    fixtures = {
        "programmer": PROGRAMMER_FIXTURE,
        "readme": DFU_README_FIXTURE,
    }

    def fake_fetch(url: str) -> str:
        return fixtures[url]

    out = fetch_nordic_usb_pids.collect(
        fetch=fake_fetch,
        programmer_url="programmer",
        dfu_readme_url="readme",
    )
    assert out["1915:521f"] == {
        "vendor": "Nordic Semiconductor ASA",
        "product": "PCA10059 nRF52840 Dongle USB SDFU bootloader",
    }
    assert out["1915:c00a"]["product"] == "Nordic USB serial DFU"
    assert out["1915:cafe"]["product"] == "Nordic USB serial DFU"
    assert out["1915:520f"]["product"] == "Nordic Thingy:91"
    assert list(out) == sorted(out)


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "nordic.json"
    old_collect = fetch_nordic_usb_pids.collect
    try:
        fetch_nordic_usb_pids.collect = lambda: {
            "1915:521f": {
                "vendor": "Nordic Semiconductor ASA",
                "product": "PCA10059 nRF52840 Dongle USB SDFU bootloader",
            }
        }
        sys.argv = ["fetch_nordic_usb_pids.py", "--out", str(out)]
        assert fetch_nordic_usb_pids.main() == 0
    finally:
        fetch_nordic_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "1915:521f": {
            "vendor": "Nordic Semiconductor ASA",
            "product": "PCA10059 nRF52840 Dongle USB SDFU bootloader",
        }
    }

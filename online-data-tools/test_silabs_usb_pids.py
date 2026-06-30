#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_silabs_usb_pids.py.

Fixtures are small, network-free snippets from Linux `cp210x.c` and a
SiliconLabsSoftware README udev-rule block.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_silabs_usb_pids  # noqa: E402


CP210X_FIXTURE = """\
static const struct usb_device_id id_table[] = {
    { USB_DEVICE(0x10C4, 0xEA60) }, /* Silicon Labs factory default */
    { USB_DEVICE(0x10C4, 0xEA61) }, /* Silicon Labs factory default */
    { USB_DEVICE(0x10C4, 0xEA63) }, /* Silicon Labs Windows Update */
    { USB_DEVICE(0x10C4, 0xEA70) }, /* Silicon Labs factory default */
    { USB_DEVICE(0x10C4, 0xEA71) }, /* Infinity GPS-MIC-1 Radio Monophone */
    { USB_DEVICE(0x10C4, 0xEA7A) }, /* Silicon Labs Windows Update */
    { USB_DEVICE(0x10C4, 0xEA7B) }, /* Silicon Labs Windows Update */
    { USB_DEVICE(0x1234, 0xEA60) },
};
"""

README_FIXTURE = """\
1. (Optional) Setup udev rules to access openocd (Linux)
   ```bash
   SUBSYSTEM=="usb", ATTRS{idVendor}=="2544", ATTRS{idProduct}=="0001", GROUP="plugdev", TAG+="uaccess"
   ```
"""


def test_parse_cp210x_driver_pids_filters_to_silabs_vid() -> None:
    pids = fetch_silabs_usb_pids.parse_cp210x_driver_pids(CP210X_FIXTURE)
    assert "ea60" in pids
    assert "ea71" in pids
    assert len(pids) == 7


def test_parse_energy_micro_udev_pids_reads_readme_rule() -> None:
    pids = fetch_silabs_usb_pids.parse_energy_micro_udev_pids(README_FIXTURE)
    assert pids == {"0001"}


def test_build_cp210x_supplement_returns_selected_bridge_rows() -> None:
    pids = fetch_silabs_usb_pids.parse_cp210x_driver_pids(CP210X_FIXTURE)
    out = fetch_silabs_usb_pids.build_cp210x_supplement(pids)
    assert out["ea60"] == "CP210x UART Bridge"
    assert out["ea70"] == "CP2105 Dual UART Bridge"
    assert out["ea71"] == "CP2108 Quad UART Bridge"
    assert out["ea7b"] == "CP2108 Quad UART Bridge"


def test_build_cp210x_supplement_rejects_missing_expected_pid() -> None:
    pids = fetch_silabs_usb_pids.parse_cp210x_driver_pids(
        CP210X_FIXTURE.replace("    { USB_DEVICE(0x10C4, 0xEA7B) },", "")
    )
    with pytest.raises(ValueError, match="missing expected PIDs"):
        fetch_silabs_usb_pids.build_cp210x_supplement(pids)


def test_build_energy_micro_supplement_labels_openocd_interface() -> None:
    pids = fetch_silabs_usb_pids.parse_energy_micro_udev_pids(README_FIXTURE)
    assert fetch_silabs_usb_pids.build_energy_micro_supplement(pids) == {
        "0001": "Silicon Labs OpenOCD debug interface"
    }


def test_collect_emits_merge_sources_shape() -> None:
    fixtures = {
        "cp210x": CP210X_FIXTURE,
        "readme": README_FIXTURE,
    }

    def fake_fetch(url: str) -> str:
        return fixtures[url]

    out = fetch_silabs_usb_pids.collect(
        fetch=fake_fetch,
        cp210x_url="cp210x",
        openocd_readme_url="readme",
    )
    assert out["10c4:ea71"] == {
        "vendor": "Silicon Labs",
        "product": "CP2108 Quad UART Bridge",
    }
    assert out["2544:0001"] == {
        "vendor": "Energy Micro AS",
        "product": "Silicon Labs OpenOCD debug interface",
    }
    assert list(out) == sorted(out)


def test_collect_keeps_cp210x_rows_when_openocd_source_fails() -> None:
    def fake_fetch(url: str) -> str:
        if url == "readme":
            raise OSError("offline")
        return CP210X_FIXTURE

    out = fetch_silabs_usb_pids.collect(
        fetch=fake_fetch,
        cp210x_url="cp210x",
        openocd_readme_url="readme",
    )
    assert "10c4:ea60" in out
    assert "2544:0001" not in out


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "silabs.json"
    old_collect = fetch_silabs_usb_pids.collect
    try:
        fetch_silabs_usb_pids.collect = lambda: {
            "2544:0001": {
                "vendor": "Energy Micro AS",
                "product": "Silicon Labs OpenOCD debug interface",
            }
        }
        sys.argv = ["fetch_silabs_usb_pids.py", "--out", str(out)]
        assert fetch_silabs_usb_pids.main() == 0
    finally:
        fetch_silabs_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "2544:0001": {
            "vendor": "Energy Micro AS",
            "product": "Silicon Labs OpenOCD debug interface",
        }
    }

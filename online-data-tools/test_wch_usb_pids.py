#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_wch_usb_pids.py.

Fixtures are small, network-free snippets from WCH's CH343 Linux driver and
udev rules.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_wch_usb_pids  # noqa: E402


DRIVER_FIXTURE = """\
static const struct usb_device_id ch343_ids[] = {
    { USB_DEVICE(0x1a86, 0x55d2) },
    { USB_DEVICE(0x1a86, 0x55d3) },
    { USB_DEVICE(0x1a86, 0x55d5) },
    { USB_DEVICE(0x1a86, 0x55d6) },
    { USB_DEVICE(0x1a86, 0x55da) },
    { USB_DEVICE_INTERFACE_NUMBER(0x1a86, 0x55db, 0x00) },
    { USB_DEVICE_INTERFACE_NUMBER(0x1a86, 0x55dd, 0x00) },
    { USB_DEVICE_INTERFACE_NUMBER(0x1a86, 0x55de, 0x00) },
    { USB_DEVICE_INTERFACE_NUMBER(0x1a86, 0x55e7, 0x00) },
    { USB_DEVICE(0x1a86, 0x55d8) },
    { USB_DEVICE(0x1a86, 0x55d4) },
    { USB_DEVICE(0x1a86, 0x55d7) },
    { USB_DEVICE(0x1a86, 0x55df) },
    { USB_DEVICE(0x1a86, 0x55e9) },
    { USB_DEVICE(0x1a86, 0x55ea) },
    { USB_DEVICE(0x1a86, 0x55e8) },
    { USB_DEVICE_INTERFACE_NUMBER(0x1a86, 0x55eb, 0x00) },
    { USB_DEVICE(0x1a86, 0x55ec) },
    { USB_DEVICE(0x1a86, 0x55ef) },
    { USB_DEVICE_INTERFACE_NUMBER(0x1a86, 0x5610, 0x01) },
    { USB_DEVICE(0x4348, 0x5523) },
};
"""

UDEV_FIXTURE = """\
# WCH CH342 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55d2", \\
        DRIVER=="cdc_acm"

# WCH CH343 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55d3", \\
        DRIVER=="cdc_acm"

# WCH CH344 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55d5", \\
        DRIVER=="cdc_acm"

# WCH CH347T Mode0 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55da", \\
        DRIVER=="cdc_acm"

# WCH CH347T Mode1 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55db", \\
        DRIVER=="cdc_acm"

# WCH CH347T Mode3 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55dd", \\
        DRIVER=="cdc_acm"

# WCH CH347T UART0 & UART1 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55de", \\
        DRIVER=="cdc_acm"

# WCH CH9101 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55d8", \\
        DRIVER=="cdc_acm"

# WCH CH9102 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55d4", \\
        DRIVER=="cdc_acm"

# WCH CH9103 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55d7", \\
        DRIVER=="cdc_acm"

# WCH CH9104 USB/Serial converter
ACTION=="add", SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="55df", \\
        DRIVER=="cdc_acm"
"""


def test_parse_driver_pids_reads_device_and_interface_macros() -> None:
    pids = fetch_wch_usb_pids.parse_driver_pids(DRIVER_FIXTURE)
    assert "55d2" in pids
    assert "55db" in pids
    assert "5610" in pids
    assert "5523" not in pids


def test_parse_udev_products_uses_comment_above_rule() -> None:
    products = fetch_wch_usb_pids.parse_udev_products(UDEV_FIXTURE)
    assert products["55d2"] == "WCH CH342 USB/Serial converter"
    assert products["55df"] == "WCH CH9104 USB/Serial converter"
    assert "55d6" not in products


def test_build_supplement_combines_udev_and_driver_only_rows() -> None:
    driver_pids = fetch_wch_usb_pids.parse_driver_pids(DRIVER_FIXTURE)
    udev_products = fetch_wch_usb_pids.parse_udev_products(UDEV_FIXTURE)
    out = fetch_wch_usb_pids.build_supplement(
        driver_pids=driver_pids,
        udev_products=udev_products,
    )
    assert out["55d2"] == "WCH CH342 USB/Serial converter"
    assert out["55e7"] == "WCH CH339 USB/Serial converter"
    assert out["55ef"] == "WCH CH9105 USB/Serial converter"
    assert out["5610"] == "WCH CH9433 USB/Serial converter"
    assert "55d6" not in out


def test_build_supplement_rejects_udev_pid_missing_from_driver() -> None:
    with pytest.raises(ValueError, match="udev rows missing from driver"):
        fetch_wch_usb_pids.build_supplement(
            driver_pids={"55d3", *fetch_wch_usb_pids.DRIVER_ONLY_PRODUCTS},
            udev_products={"55d2": "WCH CH342 USB/Serial converter"},
        )


def test_collect_emits_merge_sources_shape() -> None:
    fixtures = {
        "driver": DRIVER_FIXTURE,
        "udev": UDEV_FIXTURE,
    }

    def fake_fetch(url: str) -> str:
        return fixtures[url]

    out = fetch_wch_usb_pids.collect(
        fetch=fake_fetch,
        driver_url="driver",
        udev_url="udev",
    )
    assert out["1a86:55d4"] == {
        "vendor": "QinHeng Electronics",
        "product": "WCH CH9102 USB/Serial converter",
    }
    assert out["1a86:5610"]["product"] == "WCH CH9433 USB/Serial converter"
    assert "1a86:55d6" not in out
    assert list(out) == sorted(out)


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "wch.json"
    old_collect = fetch_wch_usb_pids.collect
    try:
        fetch_wch_usb_pids.collect = lambda: {
            "1a86:55d4": {
                "vendor": "QinHeng Electronics",
                "product": "WCH CH9102 USB/Serial converter",
            }
        }
        sys.argv = ["fetch_wch_usb_pids.py", "--out", str(out)]
        assert fetch_wch_usb_pids.main() == 0
    finally:
        fetch_wch_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "1a86:55d4": {
            "vendor": "QinHeng Electronics",
            "product": "WCH CH9102 USB/Serial converter",
        }
    }

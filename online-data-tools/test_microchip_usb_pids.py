#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_microchip_usb_pids.py.

Fixtures are small, network-free snippets from Microchip pyedbglib/pykitinfo,
AVRDUDE, and Arduino-style board packages.
"""

from __future__ import annotations

import io
import json
import sys
import zipfile
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_microchip_usb_pids  # noqa: E402


PYEDBGLIB_FIXTURE = """\
USB_VID_ATMEL = 0x03EB
USB_TOOL_DEVICE_PRODUCT_ID_JTAGICE3 = 0x2140
USB_TOOL_DEVICE_PRODUCT_ID_ATMELICE = 0x2141
USB_TOOL_DEVICE_PRODUCT_ID_POWERDEBUGGER = 0x2144
USB_TOOL_DEVICE_PRODUCT_ID_EDBG_A = 0x2111
USB_TOOL_DEVICE_PRODUCT_ID_MSD = 0x2169
USB_TOOL_DEVICE_PRODUCT_ID_ZERO = 0x2157
USB_TOOL_DEVICE_PRODUCT_ID_PUBLIC_EDBG_C = 0x216A
USB_TOOL_DEVICE_PRODUCT_ID_KRAKEN = 0x2170
USB_TOOL_DEVICE_PRODUCT_ID_MEDBG = 0x2145
USB_TOOL_DEVICE_PRODUCT_ID_NEDBG_HID_MSD_DGI_CDC = 0x2175
USB_TOOL_DEVICE_PRODUCT_ID_PICKIT4_HID_CDC = 0x2177
USB_TOOL_DEVICE_PRODUCT_ID_SNAP_HID_CDC = 0x2180
USB_TOOL_DEVICE_PRODUCT_ID_ICD4_HID_CDC = 0x217C
USB_TOOL_DEVICE_PRODUCT_ID_ICE4_HID_CDC = 0x2193
"""

PYKITINFO_FIXTURE = """\
MICROCHIP_VID = 0x04D8
MICROCHIP_NON_HID_TOOLS = [
    {"VID": MICROCHIP_VID, "PID": 0x9012, "Name": "MPLAB\\u00ae PICkit\\u21224"},
    {"VID": MICROCHIP_VID, "PID": 0x9018, "Name": "MPLAB\\u00ae Snap In-Circuit Debugger"},
    {"VID": MICROCHIP_VID, "PID": 0x9036, "Name": "MPLAB\\u00ae PICkit\\u2122 5", "Serial port": True},
    {"VID": 0x1234, "PID": 0x9999, "Name": "Other"},
]
"""

AVRDUDE_FIXTURE = """\
#define USB_VENDOR_ATMEL                     0x03EB
#define USB_VENDOR_MICROCHIP                 0x04D8
#define USB_DEVICE_JTAGICEMKII               0x2103
#define USB_DEVICE_AVRISPMKII                0x2104
#define USB_DEVICE_STK600                    0x2106
#define USB_DEVICE_AVRDRAGON                 0x2107
#define USB_DEVICE_JTAGICE3                  0x2110
#define USB_DEVICE_XPLAINEDPRO               0x2111
#define USB_DEVICE_JTAG3_EDBG                0x2140
#define USB_DEVICE_ATMEL_ICE                 0x2141
#define USB_DEVICE_POWERDEBUGGER             0x2144
#define USB_DEVICE_XPLAINEDMINI              0x2145
#define USB_DEVICE_PKOBN                     0x2175
#define USB_DEVICE_PICKIT4_AVR_MODE          0x2177
#define USB_DEVICE_PICKIT4_PIC_MODE          0x9012
#define USB_DEVICE_PICKIT4_PIC_MODE_BL       0x9017
#define USB_DEVICE_PICKIT5                   0x9036
#define USB_DEVICE_SNAP_AVR_MODE             0x2180
#define USB_DEVICE_SNAP_PIC_MODE             0x9018
#define USB_DEVICE_SNAP_PIC_MODE_BL          0x9019
#define USB_DEVICE_PICKIT_BASIC              0x9054
#define USB_DEVICE_PICKIT_BASIC_CDC          0x9055
#define USB_DEVICE_PICKIT_BASIC_CDC_VENDOR   0x9056
#define USB_DEVICE_PICKIT_BASIC_BL           0x9057
#define USB_DEVICE_PICKIT_BASIC_CIMSIS       0x90AB
#define USB_DEVICE_PICKIT_BASIC_CIMSIS_CDC   0x90AC
#define USB_DEVICE_PICKIT_BASIC_CIMSIS_DGI   0x90AD
#define USB_DEVICE_PICKIT_BASIC_CIMSIS_MSD   0x90AE
#define USBASP_OLD_VID                       0x03EB
#define USBASP_OLD_PID                       0xC7B4
"""

ARDUINO_BOARDS_FIXTURE = """\
arduino_zero_edbg.name=Arduino Zero (Programming Port)
arduino_zero_edbg.vid.0=0x03eb
arduino_zero_edbg.pid.0=0x2157
arduino_zero_edbg.upload_port.0.vid=0x03eb
arduino_zero_edbg.upload_port.0.pid=0x2157
mzero_pro_bl_dbg.name=Arduino M0 Pro (Programming Port)
mzero_pro_bl_dbg.vid.0=0x03eb
mzero_pro_bl_dbg.pid.0=0x2157
uno2018.name=Arduino UNO WiFi Rev2
uno2018.vid.0=0x03eb
uno2018.pid.0=0x2145
nona4809.name=Arduino Nano Every
nona4809.vid.0=0x2341
nona4809.pid.0=0x0058
"""

LOWPOWERLAB_INDEX_FIXTURE = json.dumps(
    {
        "packages": [
            {
                "name": "Moteino",
                "platforms": [
                    {
                        "name": "LowPowerLab SAMD Boards",
                        "architecture": "samd",
                        "version": "1.6.2",
                        "url": "old.zip",
                    },
                    {
                        "name": "LowPowerLab SAMD Boards",
                        "architecture": "samd",
                        "version": "1.6.3",
                        "url": "latest.zip",
                    },
                ],
            }
        ]
    }
)

LOWPOWERLAB_BOARDS_FIXTURE = """\
moteino_zero.name=Moteino M0
moteino_zero.vid.0=0x04d8
moteino_zero.pid.0=0xeee5
current_ranger.name=CurrentRanger
current_ranger.vid.0=0x04d8
current_ranger.pid.0=0xeee5
rfgateway_m4.name=RFGateway M4
rfgateway_m4.vid.0=0x04d8
rfgateway_m4.pid.0=0xeee6
"""


def make_zip(path: str, content: str) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        zf.writestr(path, content)
    return buf.getvalue()


def test_parse_pyedbglib_toolinfo_extracts_first_party_atmel_rows() -> None:
    rows = fetch_microchip_usb_pids.parse_pyedbglib_toolinfo(PYEDBGLIB_FIXTURE)
    assert rows["2141"] == "Atmel-ICE"
    assert rows["2157"] == "Arduino Zero EDBG"
    assert rows["217c"] == "MPLAB ICD 4"
    assert rows["2193"] == "MPLAB ICE 4"


def test_parse_pyedbglib_toolinfo_rejects_wrong_vid() -> None:
    with pytest.raises(ValueError, match="USB_VID_ATMEL"):
        fetch_microchip_usb_pids.parse_pyedbglib_toolinfo(
            PYEDBGLIB_FIXTURE.replace("0x03EB", "0x9999", 1)
        )


def test_parse_pykitinfo_tools_extracts_and_normalizes_microchip_rows() -> None:
    rows = fetch_microchip_usb_pids.parse_pykitinfo_tools(PYKITINFO_FIXTURE)
    assert rows == {
        "9012": "MPLAB PICkit 4",
        "9018": "MPLAB Snap In-Circuit Debugger",
        "9036": "MPLAB PICkit 5 CDC",
    }


def test_parse_avrdude_usbdevs_extracts_supplemental_rows() -> None:
    rows = fetch_microchip_usb_pids.parse_avrdude_usbdevs(AVRDUDE_FIXTURE)
    assert rows["03eb:2103"] == {
        "vendor": "Atmel Corp.",
        "product": "AVR JTAGICE mkII",
    }
    assert rows["04d8:9057"]["product"] == "MPLAB PICkit Basic Bootloader"
    assert rows["03eb:c7b4"]["product"] == "USBasp"


def test_parse_arduino_boards_collapses_duplicate_board_pids() -> None:
    rows = fetch_microchip_usb_pids.parse_arduino_boards(ARDUINO_BOARDS_FIXTURE)
    assert rows["03eb:2157"]["product"] == "Arduino Zero/M0 Pro programming port"
    assert rows["03eb:2145"]["product"] == "Arduino UNO WiFi Rev2"
    assert "2341:0058" not in rows


def test_parse_lowpowerlab_package_uses_latest_allowed_board_rows() -> None:
    archive = make_zip("MoteinoSAMD/boards.txt", LOWPOWERLAB_BOARDS_FIXTURE)

    def fake_fetch_bytes(url: str) -> bytes:
        assert url == "latest.zip"
        return archive

    rows = fetch_microchip_usb_pids.parse_lowpowerlab_package(
        LOWPOWERLAB_INDEX_FIXTURE,
        fetch_bytes=fake_fetch_bytes,
    )
    assert rows == {
        "04d8:eee5": {
            "vendor": "Microchip Technology, Inc.",
            "product": "LowPowerLab CurrentRanger / Moteino M0",
        }
    }


def test_collect_tiers_keep_first_party_stronger_than_supplemental() -> None:
    text_fixtures = {
        fetch_microchip_usb_pids.PYEDBGLIB_TOOLINFO_URL: PYEDBGLIB_FIXTURE,
        fetch_microchip_usb_pids.PYKITINFO_TOOLS_URL: PYKITINFO_FIXTURE,
        fetch_microchip_usb_pids.AVRDUDE_USBDEVS_URL: AVRDUDE_FIXTURE,
        fetch_microchip_usb_pids.ARDUINO_SAMD_BOARDS_URL: ARDUINO_BOARDS_FIXTURE,
        fetch_microchip_usb_pids.ARDUINO_MEGAAVR_BOARDS_URL: "",
        fetch_microchip_usb_pids.LOWPOWERLAB_PACKAGE_INDEX_URL: LOWPOWERLAB_INDEX_FIXTURE,
    }
    archive = make_zip("MoteinoSAMD/boards.txt", LOWPOWERLAB_BOARDS_FIXTURE)

    def fake_fetch_text(url: str) -> str:
        return text_fixtures[url]

    def fake_fetch_bytes(_url: str) -> bytes:
        return archive

    first = fetch_microchip_usb_pids.collect(
        tier="first-party",
        fetch_text=fake_fetch_text,
        fetch_bytes=fake_fetch_bytes,
    )
    supplement = fetch_microchip_usb_pids.collect(
        tier="supplemental",
        fetch_text=fake_fetch_text,
        fetch_bytes=fake_fetch_bytes,
    )
    all_rows = fetch_microchip_usb_pids.collect(
        tier="all",
        fetch_text=fake_fetch_text,
        fetch_bytes=fake_fetch_bytes,
    )

    assert first["03eb:2145"]["product"] == "mEDBG"
    assert supplement["03eb:2145"]["product"] == "Xplained Mini mEDBG"
    assert all_rows["03eb:2145"]["product"] == "mEDBG"
    assert all_rows["04d8:eee5"]["product"] == "LowPowerLab CurrentRanger / Moteino M0"


def test_main_emits_requested_tier(tmp_path: Path) -> None:
    out = tmp_path / "microchip.json"
    old_collect = fetch_microchip_usb_pids.collect
    try:
        fetch_microchip_usb_pids.collect = lambda tier="all": {
            "03eb:2141": {"vendor": "Atmel Corp.", "product": f"{tier}: Atmel-ICE"}
        }
        sys.argv = [
            "fetch_microchip_usb_pids.py",
            "--tier",
            "first-party",
            "--out",
            str(out),
        ]
        assert fetch_microchip_usb_pids.main() == 0
    finally:
        fetch_microchip_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "03eb:2141": {
            "vendor": "Atmel Corp.",
            "product": "first-party: Atmel-ICE",
        }
    }

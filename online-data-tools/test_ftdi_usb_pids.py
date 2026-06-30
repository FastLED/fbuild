#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_ftdi_usb_pids.py.

Fixtures are small, network-free snippets from Linux `ftdi_sio_ids.h`.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_ftdi_usb_pids  # noqa: E402


FTDI_IDS_FIXTURE = """\
#define FTDI_VID 0x0403 /* Vendor Id */
#define FTDI_8U232AM_PID 0x6001 /* Similar device to SIO above */
#define FTDI_8U232AM_ALT_PID 0x6006 /* FTDI's alternate PID for above */
#define FTDI_8U2232C_PID 0x6010 /* Dual channel device */
#define FTDI_4232H_PID 0x6011 /* Quad channel hi-speed device */
#define FTDI_232H_PID  0x6014 /* Single channel hi-speed device */
#define FTDI_FTX_PID   0x6015 /* FT-X series */
#define FTDI_FT2233HP_PID 0x6040 /* Dual channel hi-speed device with PD */
#define FTDI_FT4233HP_PID 0x6041 /* Quad channel hi-speed device with PD */
#define FTDI_FT2232HP_PID 0x6042 /* Dual channel hi-speed device with PD */
#define FTDI_FT4232HP_PID 0x6043 /* Quad channel hi-speed device with PD */
#define FTDI_FT233HP_PID 0x6044 /* Dual channel hi-speed device with PD */
#define FTDI_FT232HP_PID 0x6045 /* Dual channel hi-speed device with PD */
#define FTDI_FT4232HA_PID 0x6048 /* Quad channel automotive grade hi-speed device */
#define FTDI_SIO_PID 0x8372 /* Product Id SIO application of 8U100AX */
#define FTDI_232RL_PID 0xFBFA /* Product ID for FT232RL */
/*** third-party PIDs (using FTDI_VID) ***/
#define FTDI_BRICK_PID 0x0000
"""


def test_parse_original_ftdi_defines_stops_before_third_party() -> None:
    defines = fetch_ftdi_usb_pids.parse_original_ftdi_defines(FTDI_IDS_FIXTURE)
    assert defines["FTDI_8U232AM_ALT_PID"].pid == "6006"
    assert defines["FTDI_232RL_PID"].pid == "fbfa"
    assert "FTDI_BRICK_PID" not in defines


def test_parse_original_ftdi_defines_rejects_wrong_vendor() -> None:
    with pytest.raises(ValueError, match="does not declare FTDI_VID 0x0403"):
        fetch_ftdi_usb_pids.parse_original_ftdi_defines(
            FTDI_IDS_FIXTURE.replace("0x0403", "0x9999", 1)
        )


def test_build_supplement_filters_to_selected_original_ftdi_rows() -> None:
    defines = fetch_ftdi_usb_pids.parse_original_ftdi_defines(FTDI_IDS_FIXTURE)
    out = fetch_ftdi_usb_pids.build_supplement(defines)
    assert out == {
        "6006": "8U232AM alternate PID",
        "6040": "FT2233HP Dual channel hi-speed device with PD",
        "6041": "FT4233HP Quad channel hi-speed device with PD",
        "6042": "FT2232HP Dual channel hi-speed device with PD",
        "6043": "FT4232HP Quad channel hi-speed device with PD",
        "6044": "FT233HP Dual channel hi-speed device with PD",
        "6045": "FT232HP Dual channel hi-speed device with PD",
        "6048": "FT4232HA Quad channel automotive grade hi-speed device",
        "8372": "SIO application of 8U100AX",
        "fbfa": "FT232RL",
    }


def test_build_supplement_rejects_missing_expected_define() -> None:
    defines = fetch_ftdi_usb_pids.parse_original_ftdi_defines(
        FTDI_IDS_FIXTURE.replace("#define FTDI_232RL_PID 0xFBFA", "")
    )
    with pytest.raises(ValueError, match="missing expected defines"):
        fetch_ftdi_usb_pids.build_supplement(defines)


def test_collect_emits_merge_sources_shape() -> None:
    def fake_fetch(_url: str) -> str:
        return FTDI_IDS_FIXTURE

    out = fetch_ftdi_usb_pids.collect(fetch=fake_fetch, url="header")
    assert out["0403:6040"] == {
        "vendor": "Future Technology Devices International, Ltd",
        "product": "FT2233HP Dual channel hi-speed device with PD",
    }
    assert out["0403:fbfa"]["product"] == "FT232RL"
    assert "0403:6001" not in out
    assert "0403:0000" not in out
    assert list(out) == sorted(out)


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "ftdi.json"
    old_collect = fetch_ftdi_usb_pids.collect
    try:
        fetch_ftdi_usb_pids.collect = lambda: {
            "0403:6040": {
                "vendor": "Future Technology Devices International, Ltd",
                "product": "FT2233HP Dual channel hi-speed device with PD",
            }
        }
        sys.argv = ["fetch_ftdi_usb_pids.py", "--out", str(out)]
        assert fetch_ftdi_usb_pids.main() == 0
    finally:
        fetch_ftdi_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "0403:6040": {
            "vendor": "Future Technology Devices International, Ltd",
            "product": "FT2233HP Dual channel hi-speed device with PD",
        }
    }

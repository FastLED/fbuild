#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_stm_usb_pids.py.

Fixtures are small, network-free snippets from ST/OpenOCD ST-LINK sources.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_stm_usb_pids  # noqa: E402


STLINK_DRIVER_FIXTURE = """\
#define STLINK_V1_PID         (0x3744)
#define STLINK_V2_PID         (0x3748)
#define STLINK_V2_1_PID       (0x374B)
#define STLINK_V2_1_NO_MSD_PID  (0x3752)
#define STLINK_V3_USBLOADER_PID (0x374D)
#define STLINK_V3E_PID          (0x374E)
#define STLINK_V3S_PID          (0x374F)
#define STLINK_V3_2VCP_PID      (0x3753)
#define STLINK_V3E_NO_MSD_PID   (0x3754)
#define STLINK_V3P_USBLOADER_PID (0x3755)
#define STLINK_V3P_PID           (0x3757)
"""

STLINK_CFG_FIXTURE = """\
hla_vid_pid 0x0483 0x3744 0x0483 0x3748 0x0483 0x374b 0x0483 0x374d \\
  0x0483 0x374e 0x0483 0x374f 0x0483 0x3752 0x0483 0x3753 \\
  0x0483 0x3754 0x0483 0x3755 0x0483 0x3757
"""


def test_parse_stlink_defines_extracts_expected_pids() -> None:
    defines = fetch_stm_usb_pids.parse_stlink_defines(STLINK_DRIVER_FIXTURE)
    assert defines["STLINK_V3E_NO_MSD_PID"] == "3754"
    assert defines["STLINK_V3P_USBLOADER_PID"] == "3755"
    assert defines["STLINK_V3P_PID"] == "3757"


def test_parse_stlink_defines_rejects_missing_expected_define() -> None:
    with pytest.raises(ValueError, match="missing expected defines"):
        fetch_stm_usb_pids.parse_stlink_defines(
            STLINK_DRIVER_FIXTURE.replace("#define STLINK_V3P_PID", "#define OTHER")
        )


def test_parse_stlink_cfg_pids_extracts_st_vid_pairs() -> None:
    pids = fetch_stm_usb_pids.parse_stlink_cfg_pids(
        STLINK_CFG_FIXTURE + "\n0x9999 0x3757\n"
    )
    assert "3744" in pids
    assert "3757" in pids
    assert len(pids) == 11


def test_build_supplement_adds_common_st_rows_and_stlink_names() -> None:
    defines = fetch_stm_usb_pids.parse_stlink_defines(STLINK_DRIVER_FIXTURE)
    cfg_pids = fetch_stm_usb_pids.parse_stlink_cfg_pids(STLINK_CFG_FIXTURE)
    out = fetch_stm_usb_pids.build_supplement(defines=defines, cfg_pids=cfg_pids)
    assert out["df11"] == "STM Device in DFU Mode"
    assert out["5740"] == "Virtual COM Port"
    assert out["3754"] == "STLINK-V3E no MSD"
    assert out["3755"] == "STLINK-V3P USB loader"
    assert out["3757"] == "STLINK-V3P"


def test_build_supplement_rejects_cfg_missing_driver_pid() -> None:
    defines = fetch_stm_usb_pids.parse_stlink_defines(STLINK_DRIVER_FIXTURE)
    with pytest.raises(ValueError, match="cfg missing driver PID"):
        fetch_stm_usb_pids.build_supplement(defines=defines, cfg_pids={"3744"})


def test_collect_emits_merge_sources_shape() -> None:
    fixtures = {
        "driver": STLINK_DRIVER_FIXTURE,
        "cfg": STLINK_CFG_FIXTURE,
    }

    def fake_fetch(url: str) -> str:
        return fixtures[url]

    out = fetch_stm_usb_pids.collect(
        fetch=fake_fetch,
        driver_url="driver",
        cfg_url="cfg",
    )
    assert out["0483:3757"] == {
        "vendor": "STMicroelectronics",
        "product": "STLINK-V3P",
    }
    assert out["0483:df11"]["product"] == "STM Device in DFU Mode"
    assert list(out) == sorted(out)


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "stm.json"
    old_collect = fetch_stm_usb_pids.collect
    try:
        fetch_stm_usb_pids.collect = lambda: {
            "0483:3757": {
                "vendor": "STMicroelectronics",
                "product": "STLINK-V3P",
            }
        }
        sys.argv = ["fetch_stm_usb_pids.py", "--out", str(out)]
        assert fetch_stm_usb_pids.main() == 0
    finally:
        fetch_stm_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "0483:3757": {
            "vendor": "STMicroelectronics",
            "product": "STLINK-V3P",
        }
    }

#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_nxp_usb_pids.py.

Fixtures are small, network-free snippets from NXP mfgtools `config.cpp`.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_nxp_usb_pids  # noqa: E402


MFGTOOLS_FIXTURE = """\
constexpr uint16_t NXP_VID = 0x1FC9;
	emplace_back(ConfigItem{"SDPS:", "MX8QXP", nullptr,   NXP_VID, 0x012F, 0x0002});
	emplace_back(ConfigItem{"SDP:", "MX7ULP",   nullptr,  NXP_VID, 0x0126});
	emplace_back(ConfigItem{"SDP:", "MXRT106X",  nullptr,  NXP_VID, 0x0135});
	emplace_back(ConfigItem{"SDPV:", "SPL1",   "SPL",  NXP_VID, 0x0151, 0x0500, 0x9998});
	emplace_back(ConfigItem{"FBK:", nullptr, nullptr, NXP_VID, 0x0153});
	emplace_back(ConfigItem{"FB:", nullptr, nullptr,  NXP_VID, 0x0152});
"""


def test_product_name_labels_protocol_modes() -> None:
    assert (
        fetch_nxp_usb_pids.product_name(protocol="SDP:", name="MXRT106X")
        == "NXP MXRT106X serial downloader (SDP)"
    )
    assert fetch_nxp_usb_pids.product_name(protocol="FB:", name=None) == "NXP fastboot"
    assert (
        fetch_nxp_usb_pids.product_name(protocol="FBK:", name=None)
        == "NXP fastboot kernel"
    )


def test_parse_mfgtools_config_extracts_nxp_rows() -> None:
    parsed = fetch_nxp_usb_pids.parse_mfgtools_config(MFGTOOLS_FIXTURE)
    assert parsed == {
        "0126": "NXP MX7ULP serial downloader (SDP)",
        "012f": "NXP MX8QXP serial downloader (SDPS)",
        "0135": "NXP MXRT106X serial downloader (SDP)",
        "0151": "NXP SPL1 serial downloader (SDPV)",
        "0152": "NXP fastboot",
        "0153": "NXP fastboot kernel",
    }


def test_parse_mfgtools_config_rejects_wrong_vid() -> None:
    with pytest.raises(ValueError, match="does not declare NXP_VID"):
        fetch_nxp_usb_pids.parse_mfgtools_config(
            MFGTOOLS_FIXTURE.replace("0x1FC9", "0x9999")
        )


def test_parse_mfgtools_config_rejects_conflicting_duplicate() -> None:
    text = MFGTOOLS_FIXTURE + (
        '\templace_back(ConfigItem{"SDP:", "OTHER", nullptr, NXP_VID, 0x0135});\n'
    )
    with pytest.raises(ValueError, match="duplicate NXP PID"):
        fetch_nxp_usb_pids.parse_mfgtools_config(text)


def test_collect_emits_merge_sources_shape() -> None:
    def fake_fetch(_url: str) -> str:
        return MFGTOOLS_FIXTURE

    out = fetch_nxp_usb_pids.collect(fetch=fake_fetch, url="config")
    assert out["1fc9:0135"] == {
        "vendor": "NXP Semiconductors",
        "product": "NXP MXRT106X serial downloader (SDP)",
    }
    assert out["1fc9:0152"]["product"] == "NXP fastboot"
    assert list(out) == sorted(out)


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "nxp.json"
    old_collect = fetch_nxp_usb_pids.collect
    try:
        fetch_nxp_usb_pids.collect = lambda: {
            "1fc9:0135": {
                "vendor": "NXP Semiconductors",
                "product": "NXP MXRT106X serial downloader (SDP)",
            }
        }
        sys.argv = ["fetch_nxp_usb_pids.py", "--out", str(out)]
        assert fetch_nxp_usb_pids.main() == 0
    finally:
        fetch_nxp_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "1fc9:0135": {
            "vendor": "NXP Semiconductors",
            "product": "NXP MXRT106X serial downloader (SDP)",
        }
    }

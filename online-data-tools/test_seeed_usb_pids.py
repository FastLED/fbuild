#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_seeed_usb_pids.py."""

from __future__ import annotations

import io
import json
import sys
import tarfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_seeed_usb_pids  # noqa: E402


BOARDS_FIXTURE = """\
seeed_XIAO_m0.name=Seeeduino XIAO
seeed_XIAO_m0.vid.0=0x2886
seeed_XIAO_m0.pid.0=0x802F
seeed_XIAO_m0.vid.1=0x2886
seeed_XIAO_m0.pid.1=0x002F
xiao_mg24.name=Seeed Studio XIAO MG24 (Sense)
xiao_mg24.vid.0=0x2886
xiao_mg24.pid.0=0x0062
xiao_mg24.upload_port.0.vid=0x2886
xiao_mg24.upload_port.0.pid=0x8062
arduino_zero.name=Arduino Zero
arduino_zero.vid.0=0x2341
arduino_zero.pid.0=0x004D
"""

INDEX_FIXTURE = {
    "packages": [
        {
            "platforms": [
                {
                    "name": "Seeed SAMD Boards",
                    "architecture": "samd",
                    "version": "1.8.5",
                    "url": "https://files.seeedstudio.com/old.tar.bz2",
                },
                {
                    "name": "Seeed SAMD Boards",
                    "architecture": "samd",
                    "version": "1.8.6",
                    "url": "https://files.seeedstudio.com/new.tar.bz2",
                },
                {
                    "name": "Seeed nRF52 mbed-enabled Boards",
                    "architecture": "mbed",
                    "version": "2.6.1",
                    "url": "http://127.0.0.1:8000/local.tar.bz2",
                },
            ]
        }
    ]
}

PLATFORM_C3_FIXTURE = {
    "build": {"hwids": [["0x2886", "0x0046"], ["0x303A", "0x1001"]]},
    "name": "Seeed Studio XIAO ESP32C3",
    "vendor": "Seeed Studio",
}

PLATFORM_C6_CONFLICT_FIXTURE = {
    "build": {"hwids": [["0x2886", "0x0046"], ["0x303A", "0x1001"]]},
    "name": "Seeed Studio XIAO ESP32C6",
    "vendor": "Seeed Studio",
}

MAKEBOARDS_FIXTURE = """\
MakeBoard("seeed_xiao_rp2040", "rp2040", "Seeed", "XIAO RP2040", "0x2e8a", "0x000a", 250, "SEEED_XIAO_RP2040", 2, 0, "boot2")
MakeBoard("seeed_xiao_rp2350", "rp2350", "Seeed", "XIAO RP2350", "0x2886", "0x0058", 250, "SEEED_XIAO_RP2350", 2, 0, "none")
"""

CP_FIXTURE = """\
USB_VID = 0x2886
USB_PID = 0x0042
USB_PRODUCT = "XIAO RP2040"
USB_MANUFACTURER = "Seeed Studio"
"""


def _tar_with_boards(text: str) -> bytes:
    out = io.BytesIO()
    data = text.encode()
    with tarfile.open(fileobj=out, mode="w:bz2") as tf:
        info = tarfile.TarInfo("core/boards.txt")
        info.size = len(data)
        tf.addfile(info, io.BytesIO(data))
    return out.getvalue()


def test_parse_boards_txt_keeps_direct_and_upload_port_rows() -> None:
    rows = fetch_seeed_usb_pids.parse_boards_txt(BOARDS_FIXTURE)

    assert rows["2886:802f"]["product"] == "Seeeduino XIAO"
    assert rows["2886:002f"]["product"] == "Seeeduino XIAO"
    assert rows["2886:0062"]["product"] == "Seeed Studio XIAO MG24 (Sense)"
    assert rows["2886:8062"]["product"] == "Seeed Studio XIAO MG24 (Sense)"
    assert "2341:004d" not in rows


def test_latest_package_sources_selects_latest_https_archive() -> None:
    sources = fetch_seeed_usb_pids.latest_package_sources(json.dumps(INDEX_FIXTURE))

    assert sources == [
        fetch_seeed_usb_pids.PackageSource(
            "Seeed SAMD Boards",
            "samd",
            "1.8.6",
            "https://files.seeedstudio.com/new.tar.bz2",
        )
    ]


def test_parse_package_archive_extracts_boards_txt() -> None:
    rows = fetch_seeed_usb_pids.parse_package_archive(_tar_with_boards(BOARDS_FIXTURE))

    assert rows["2886:802f"]["product"] == "Seeeduino XIAO"


def test_parse_board_json_filters_vid_and_conflicting_c6_row() -> None:
    c3_rows = fetch_seeed_usb_pids.parse_board_json(
        json.dumps(PLATFORM_C3_FIXTURE),
        skip_conflicts=True,
    )
    assert c3_rows["2886:0046"]["product"] == "Seeed Studio XIAO ESP32C3"

    c6_rows = fetch_seeed_usb_pids.parse_board_json(
        json.dumps(PLATFORM_C6_CONFLICT_FIXTURE),
        skip_conflicts=True,
    )
    assert c6_rows == {}


def test_parse_arduino_pico_skips_rp2040_parent_vid() -> None:
    rows = fetch_seeed_usb_pids.parse_arduino_pico_makeboards(MAKEBOARDS_FIXTURE)

    assert rows == {
        "2886:0058": {
            "vendor": "Seeed Technology Co., Ltd.",
            "product": "Seeed XIAO RP2350",
        }
    }


def test_descriptor_tree_skips_xiao_rp2040_path() -> None:
    def fake_json(_url: str) -> dict:
        return {
            "tree": [
                {"path": "ports/raspberrypi/boards/seeed_xiao_rp2040/mpconfigboard.mk"}
            ]
        }

    def fake_text(_url: str) -> str:
        return CP_FIXTURE

    rows = fetch_seeed_usb_pids.collect_descriptor_tree(
        name="cp",
        tree_url="tree",
        raw_base="https://raw",
        syntax="make",
        path_filter=fetch_seeed_usb_pids._circuitpython_board_paths,
        fetch_text=fake_text,
        fetch_json=fake_json,
    )

    assert rows == {}


def test_merge_fill_gaps_keeps_first_party_over_weak() -> None:
    first_party = {
        "2886:0048": {
            "vendor": "Seeed Technology Co., Ltd.",
            "product": "First-party C6",
        }
    }
    weak = {
        "2886:0048": {
            "vendor": "Seeed Technology Co., Ltd.",
            "product": "Weak C6",
        },
        "2886:8048": {
            "vendor": "Seeed Technology Co., Ltd.",
            "product": "Weak bootloader C6",
        },
    }

    assert fetch_seeed_usb_pids._merge_fill_gaps(first_party, weak) == {
        "2886:0048": {
            "vendor": "Seeed Technology Co., Ltd.",
            "product": "First-party C6",
        },
        "2886:8048": {
            "vendor": "Seeed Technology Co., Ltd.",
            "product": "Weak bootloader C6",
        },
    }


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "seeed.json"
    old_collect = fetch_seeed_usb_pids.collect
    try:
        fetch_seeed_usb_pids.collect = lambda tier="all": {
            "2886:802f": {
                "vendor": "Seeed Technology Co., Ltd.",
                "product": "Seeeduino XIAO",
            }
        }
        sys.argv = ["fetch_seeed_usb_pids.py", "--out", str(out)]
        assert fetch_seeed_usb_pids.main() == 0
    finally:
        fetch_seeed_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "2886:802f": {
            "vendor": "Seeed Technology Co., Ltd.",
            "product": "Seeeduino XIAO",
        }
    }

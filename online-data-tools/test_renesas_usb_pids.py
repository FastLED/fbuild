#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_renesas_usb_pids.py."""

from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_renesas_usb_pids  # noqa: E402


BOARDS_FIXTURE = """\
portenta_c33.name=Arduino Portenta C33
portenta_c33.vid.0=0x2341
portenta_c33.pid.0=0x0068
portenta_c33.vid.1=0x2341
portenta_c33.pid.1=0x0368

minima.name=Arduino UNO R4 Minima
minima.vid.0=0x2341
minima.pid.0=0x0069
minima.vid.1=0x2341
minima.pid.1=0x0369

unor4wifi.name=Arduino UNO R4 WiFi
unor4wifi.vid.0=0x2341
unor4wifi.pid.0=0x1002
unor4wifi.vid.1=0x2341
unor4wifi.pid.1=0x006D

nanor4.name=Arduino Nano R4
nanor4.vid.0=0x2341
nanor4.pid.0=0x0074
nanor4.vid.1=0x2341
nanor4.pid.1=0x0374

renesas_native.name=Renesas Native Tool
renesas_native.vid.0=0x045b
renesas_native.pid.0=0x0261
"""


def test_parse_boards_txt_extracts_arduino_renesas_rows() -> None:
    rows = fetch_renesas_usb_pids.parse_boards_txt(BOARDS_FIXTURE)

    assert rows["2341:0068"] == {
        "vendor": "Arduino SA",
        "product": "Arduino Portenta C33",
    }
    assert rows["2341:0069"]["product"] == "Arduino UNO R4 Minima"
    assert rows["2341:006d"]["product"] == "Arduino UNO R4 WiFi"
    assert rows["2341:0074"]["product"] == "Arduino Nano R4"
    assert "045b:0261" not in rows
    assert list(rows) == sorted(rows)


def test_parse_boards_txt_collapses_duplicate_pid_names() -> None:
    fixture = BOARDS_FIXTURE + """\
minima_alias.name=Arduino UNO R4 Minima Alternate
minima_alias.vid.0=0x2341
minima_alias.pid.0=0x0069
"""
    rows = fetch_renesas_usb_pids.parse_boards_txt(fixture)

    assert (
        rows["2341:0069"]["product"]
        == "Arduino UNO R4 Minima / Arduino UNO R4 Minima Alternate"
    )


def test_collect_emits_merge_sources_shape() -> None:
    def fake_fetch(_url: str) -> str:
        return BOARDS_FIXTURE

    rows = fetch_renesas_usb_pids.collect(fetch=fake_fetch, url="boards")
    assert rows["2341:1002"] == {
        "vendor": "Arduino SA",
        "product": "Arduino UNO R4 WiFi",
    }
    assert rows["2341:0368"]["product"] == "Arduino Portenta C33"


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "renesas.json"
    old_collect = fetch_renesas_usb_pids.collect
    try:
        fetch_renesas_usb_pids.collect = lambda: {
            "2341:0069": {
                "vendor": "Arduino SA",
                "product": "Arduino UNO R4 Minima",
            }
        }
        sys.argv = ["fetch_renesas_usb_pids.py", "--out", str(out)]
        assert fetch_renesas_usb_pids.main() == 0
    finally:
        fetch_renesas_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "2341:0069": {
            "vendor": "Arduino SA",
            "product": "Arduino UNO R4 Minima",
        }
    }

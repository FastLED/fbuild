#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for fetch_arduino_usb_pids.py."""

from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch_arduino_usb_pids  # noqa: E402


BOARDS_FIXTURE = """\
uno.name=Arduino Uno
uno.vid.0=0x2341
uno.pid.0=0x0043
uno.vid.1=0x2A03
uno.pid.1=0x0043

leonardo.name=Arduino Leonardo
leonardo.vid.0=0x2341
leonardo.pid.0=0x8036

mkrwifi1010.name=Arduino MKR WiFi 1010
mkrwifi1010.vid.0=0x2341
mkrwifi1010.pid.0=0x8057

zero_edbg.name=Arduino Zero (Programming Port)
zero_edbg.vid.0=0x03eb
zero_edbg.pid.0=0x2157
"""


def test_parse_boards_txt_extracts_arduino_vids_only() -> None:
    rows = fetch_arduino_usb_pids.parse_boards_txt(BOARDS_FIXTURE)

    assert rows["2341:0043"] == {
        "vendor": "Arduino SA",
        "product": "Arduino Uno",
    }
    assert rows["2a03:0043"] == {
        "vendor": "Arduino LLC",
        "product": "Arduino Uno",
    }
    assert rows["2341:8036"]["product"] == "Arduino Leonardo"
    assert rows["2341:8057"]["product"] == "Arduino MKR WiFi 1010"
    assert "03eb:2157" not in rows
    assert list(rows) == sorted(rows)


def test_parse_boards_txt_collapses_duplicate_pid_names() -> None:
    fixture = BOARDS_FIXTURE + """\
uno_clone.name=Arduino Uno Compatible
uno_clone.vid.0=0x2341
uno_clone.pid.0=0x0043
"""
    rows = fetch_arduino_usb_pids.parse_boards_txt(fixture)

    assert rows["2341:0043"]["product"] == "Arduino Uno / Arduino Uno Compatible"


def test_collect_uses_first_official_source_on_conflict() -> None:
    first = fetch_arduino_usb_pids.BoardSource("first", "first")
    second = fetch_arduino_usb_pids.BoardSource("second", "second")

    def fake_fetch(url: str) -> str:
        if url == "first":
            return BOARDS_FIXTURE
        return """\
uno.name=Different Uno Name
uno.vid.0=0x2341
uno.pid.0=0x0043
nano_matter.name=Arduino Nano Matter
nano_matter.vid.0=0x2341
nano_matter.pid.0=0x0072
"""

    rows = fetch_arduino_usb_pids.collect(
        fetch=fake_fetch,
        sources=(first, second),
    )

    assert rows["2341:0043"]["product"] == "Arduino Uno"
    assert rows["2341:0072"]["product"] == "Arduino Nano Matter"


def test_main_emits_merge_sources_shape(tmp_path: Path) -> None:
    out = tmp_path / "arduino.json"
    old_collect = fetch_arduino_usb_pids.collect
    try:
        fetch_arduino_usb_pids.collect = lambda: {
            "2341:0043": {
                "vendor": "Arduino SA",
                "product": "Arduino Uno",
            }
        }
        sys.argv = ["fetch_arduino_usb_pids.py", "--out", str(out)]
        assert fetch_arduino_usb_pids.main() == 0
    finally:
        fetch_arduino_usb_pids.collect = old_collect

    data = json.loads(out.read_text(encoding="utf-8"))
    assert data == {
        "2341:0043": {
            "vendor": "Arduino SA",
            "product": "Arduino Uno",
        }
    }

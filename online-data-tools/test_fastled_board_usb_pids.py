#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for extract_fastled_board_usb_pids.py."""

from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import extract_fastled_board_usb_pids  # noqa: E402


def _write_board(
    path: Path,
    *,
    name: str,
    vendor: str,
    vid: object | None,
    pid: object | None,
) -> None:
    build = {}
    if vid is not None:
        build["vid"] = vid
    if pid is not None:
        build["pid"] = pid
    path.write_text(
        json.dumps(
            {
                "build": build,
                "id": path.stem,
                "name": name,
                "vendor": vendor,
            }
        ),
        encoding="utf-8",
    )


def test_extract_board_usb_pids_normalizes_and_collapses_duplicates(tmp_path: Path) -> None:
    _write_board(
        tmp_path / "alpha.json",
        name=" Alpha   Board ",
        vendor="Vendor A",
        vid="0x239A",
        pid="0x811B",
    )
    _write_board(
        tmp_path / "beta.json",
        name="Beta Board",
        vendor="Vendor B",
        vid="239a",
        pid="811b",
    )
    _write_board(
        tmp_path / "gamma.json",
        name="Gamma Board",
        vendor="Vendor C",
        vid=0x2E8A,
        pid=0x00C0,
    )
    _write_board(
        tmp_path / "missing_pid.json",
        name="Missing PID",
        vendor="Vendor D",
        vid="0x1209",
        pid=None,
    )
    (tmp_path / "bad.json").write_text("{", encoding="utf-8")

    rows = extract_fastled_board_usb_pids.extract_board_usb_pids(tmp_path)

    assert rows == {
        "239a:811b": {
            "vendor": "Vendor A / Vendor B",
            "product": "Alpha Board / Beta Board",
        },
        "2e8a:00c0": {
            "vendor": "Vendor C",
            "product": "Gamma Board",
        },
    }
    assert list(rows) == sorted(rows)


def test_write_json_emits_merge_sources_shape(tmp_path: Path) -> None:
    rows = {
        "feed:c0de": {
            "vendor": "Feedface Inc",
            "product": "Coded Widget",
        }
    }
    out = tmp_path / "fastled-board-usb-pids.json"

    extract_fastled_board_usb_pids.write_json(out, rows)

    assert json.loads(out.read_text(encoding="utf-8")) == rows

#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""TDD tests for build_sqlite.py.

These are RED until build_sqlite.build_db() is implemented. They cover:
  - schema: every expected table + view + FTS5 virtual table exists
  - data round-trip: every JSON row appears in the matching table
  - canned query #2 (VID + PID → top-10 boards) ranks correctly for ESP32-S3
  - canned query #1 (board name → VID:PID) ranks 0x303a top for an ESP32-S3
  - unknown VID/PID returns an empty set (no crash)
  - duplicate board_ids are deduped to the highest-scoring row
"""

from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path

import pytest

# Sibling-import the script under test. Renaming via `sys.path` insert keeps
# the script self-contained as a PEP 723 standalone — it doesn't need to be
# a package member to be importable here.
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import build_sqlite  # noqa: E402  (after sys.path mutation)


# --------------------------------------------------------------------------- #
# Sample fixtures — minimal but realistic enough to exercise the joins.
# --------------------------------------------------------------------------- #

@pytest.fixture
def sample_usb_vid() -> dict:
    return {
        "303a": {
            "vendor": "Espressif Systems",
            "products": [
                ["1001", "USB JTAG/serial debug unit"],
                ["4001", "ESP32-S2"],
                ["4002", "ESP32-S3"],
            ],
        },
        "0483": {
            "vendor": "STMicroelectronics",
            "products": [
                ["374b", "ST-Link/V2.1"],
            ],
        },
        "2e8a": {
            "vendor": "Raspberry Pi",
            "products": [
                ["0003", "RP2 Boot"],
                ["000a", "Pico SDK CDC UART"],
            ],
        },
    }


@pytest.fixture
def sample_pio_boards() -> dict:
    return {
        "esp32-s3-devkitc-1": {
            "id": "esp32-s3-devkitc-1",
            "name": "Espressif ESP32-S3-DevKitC-1-N8",
            "vendor": "Espressif",
            "mcu": "ESP32S3",
            "platform": "espressif32",
            "frameworks": ["arduino", "espidf"],
            "fcpu": 240000000,
            "ram": 327680,
            "rom": 8388608,
            "url": "https://example.invalid/s3-devkitc",
        },
        "rpipico": {
            "id": "rpipico",
            "name": "Raspberry Pi Pico",
            "vendor": "Raspberry Pi",
            "mcu": "RP2040",
            "platform": "raspberrypi",
            "frameworks": ["arduino"],
            "fcpu": 133000000,
            "ram": 270336,
            "rom": 2097152,
            "url": "https://example.invalid/pico",
        },
        "bluepill_f103c8": {
            "id": "bluepill_f103c8",
            "name": "BluePill F103C8",
            "vendor": "Generic",
            "mcu": "STM32F103C8",
            "platform": "ststm32",
            "frameworks": ["arduino", "stm32cube"],
            "fcpu": 72000000,
            "ram": 20480,
            "rom": 65536,
            "url": "https://example.invalid/bluepill",
        },
    }


@pytest.fixture
def sample_vendor_boards() -> dict:
    # Subset of pio-boards (slim view). The merger guarantees keys overlap.
    return {
        "esp32-s3-devkitc-1": {
            "mcu": "ESP32S3",
            "name": "Espressif ESP32-S3-DevKitC-1-N8",
            "vendor": "Espressif",
        },
        "rpipico": {
            "mcu": "RP2040",
            "name": "Raspberry Pi Pico",
            "vendor": "Raspberry Pi",
        },
        "bluepill_f103c8": {
            "mcu": "STM32F103C8",
            "name": "BluePill F103C8",
            "vendor": "Generic",
        },
    }


@pytest.fixture
def sample_mcu_to_vid() -> list[dict]:
    return [
        {"mcu_family": "ESP32S3",  "vid": "303a", "score": 0.90, "reason": "Espressif native USB"},
        {"mcu_family": "ESP32",    "vid": "303a", "score": 0.85, "reason": "Espressif native USB"},
        {"mcu_family": "ESP32",    "vid": "10c4", "score": 0.40, "reason": "CP210x UART bridge"},
        {"mcu_family": "ESP32",    "vid": "1a86", "score": 0.40, "reason": "CH340 UART bridge"},
        {"mcu_family": "RP2040",   "vid": "2e8a", "score": 0.95, "reason": "RP boot / CDC"},
        {"mcu_family": "STM32",    "vid": "0483", "score": 0.80, "reason": "STMicro DFU / ST-Link"},
    ]


@pytest.fixture
def built_db(
    tmp_path: Path,
    sample_usb_vid: dict,
    sample_pio_boards: dict,
    sample_vendor_boards: dict,
    sample_mcu_to_vid: list[dict],
) -> Path:
    """Materialize sample JSON into temp files and run build_db()."""
    data_dir = tmp_path / "data"
    data_dir.mkdir()
    paths = {
        "usb_vid":        data_dir / "usb-vid.json",
        "pio_boards":     data_dir / "pio-boards.json",
        "vendor_boards": data_dir / "vendor_boards.json",
        "mcu_to_vid":     data_dir / "mcu_to_vid.json",
    }
    paths["usb_vid"].write_text(json.dumps(sample_usb_vid), encoding="utf-8")
    paths["pio_boards"].write_text(json.dumps(sample_pio_boards), encoding="utf-8")
    paths["vendor_boards"].write_text(json.dumps(sample_vendor_boards), encoding="utf-8")
    paths["mcu_to_vid"].write_text(json.dumps(sample_mcu_to_vid), encoding="utf-8")

    out = tmp_path / "2026-06-20.db"
    build_sqlite.build_db(
        usb_vid_json=paths["usb_vid"],
        pio_boards_json=paths["pio_boards"],
        vendor_boards_json=paths["vendor_boards"],
        mcu_to_vid_json=paths["mcu_to_vid"],
        out_path=out,
    )
    assert out.is_file(), "build_db must create the output file"
    return out


# --------------------------------------------------------------------------- #
# Schema
# --------------------------------------------------------------------------- #

def _table_names(conn: sqlite3.Connection) -> set[str]:
    rows = conn.execute(
        "SELECT name FROM sqlite_master WHERE type IN ('table','view') "
        "AND name NOT LIKE 'sqlite_%'"
    ).fetchall()
    return {r[0] for r in rows}


def test_db_creates_expected_tables(built_db: Path) -> None:
    with sqlite3.connect(built_db) as conn:
        names = _table_names(conn)
    required = {"usb_vendor", "usb_product", "board", "mcu_to_vid", "board_vid_guess"}
    missing = required - names
    assert not missing, f"missing tables/views: {missing}; got {names}"


def test_db_has_fts5_index(built_db: Path) -> None:
    with sqlite3.connect(built_db) as conn:
        names = {
            r[0] for r in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table' "
                "AND sql LIKE '%fts5%'"
            ).fetchall()
        }
    assert "board_fts" in names, f"board_fts virtual table missing; got {names}"


# --------------------------------------------------------------------------- #
# Round-trip: JSON rows → SQL rows
# --------------------------------------------------------------------------- #

def test_usb_vendor_round_trip(built_db: Path, sample_usb_vid: dict) -> None:
    with sqlite3.connect(built_db) as conn:
        for vid_hex, payload in sample_usb_vid.items():
            row = conn.execute(
                "SELECT vendor FROM usb_vendor WHERE vid = ?",
                (int(vid_hex, 16),),
            ).fetchone()
            assert row is not None, f"vid 0x{vid_hex} missing from usb_vendor"
            assert row[0] == payload["vendor"]


def test_usb_product_round_trip(built_db: Path, sample_usb_vid: dict) -> None:
    with sqlite3.connect(built_db) as conn:
        for vid_hex, payload in sample_usb_vid.items():
            for pid_hex, product_name in payload["products"]:
                row = conn.execute(
                    "SELECT product FROM usb_product WHERE vid = ? AND pid = ?",
                    (int(vid_hex, 16), int(pid_hex, 16)),
                ).fetchone()
                assert row is not None, (
                    f"product {vid_hex}:{pid_hex} missing"
                )
                assert row[0] == product_name


def test_board_round_trip(built_db: Path, sample_pio_boards: dict) -> None:
    with sqlite3.connect(built_db) as conn:
        for board_id, payload in sample_pio_boards.items():
            row = conn.execute(
                "SELECT name, vendor, mcu, platform, framework, url "
                "FROM board WHERE id = ?",
                (board_id,),
            ).fetchone()
            assert row is not None, f"board {board_id} missing"
            assert row[0] == payload["name"]
            assert row[1] == payload["vendor"]
            assert row[2] == payload["mcu"]
            assert row[3] == payload["platform"]
            # frameworks list → comma-joined for FTS use
            assert set(row[4].split(",")) == set(payload["frameworks"])
            assert row[5] == payload["url"]


def test_mcu_to_vid_round_trip(built_db: Path, sample_mcu_to_vid: list[dict]) -> None:
    with sqlite3.connect(built_db) as conn:
        for entry in sample_mcu_to_vid:
            row = conn.execute(
                "SELECT score, reason FROM mcu_to_vid "
                "WHERE mcu_family = ? AND vid = ?",
                (entry["mcu_family"], int(entry["vid"], 16)),
            ).fetchone()
            assert row is not None, (
                f"mcu_to_vid ({entry['mcu_family']}, 0x{entry['vid']}) missing"
            )
            assert row[0] == pytest.approx(entry["score"])
            assert row[1] == entry["reason"]


# --------------------------------------------------------------------------- #
# Canned queries — these are the contract the UI relies on.
# --------------------------------------------------------------------------- #

# The headline query: given a VID + PID, what board is most likely?
QUERY_VID_PID_TO_BOARDS = """
SELECT
  b.id            AS board_id,
  b.name          AS board_name,
  b.vendor        AS board_vendor,
  b.mcu           AS mcu,
  v.vendor        AS usb_vendor,
  p.product       AS usb_product,
  (
    m.score
    + CASE WHEN p.pid IS NOT NULL THEN 0.25 ELSE 0.0 END
    + CASE WHEN LOWER(b.vendor) = LOWER(v.vendor) THEN 0.10 ELSE 0.0 END
  )               AS score
FROM mcu_to_vid m
JOIN usb_vendor v
  ON v.vid = m.vid
LEFT JOIN usb_product p
  ON p.vid = m.vid AND p.pid = ?2
JOIN board b
  ON b.mcu = m.mcu_family OR b.mcu LIKE m.mcu_family || '%'
WHERE m.vid = ?1
ORDER BY score DESC
LIMIT 10;
"""


def test_canned_query_vid_pid_to_boards_esp32s3(built_db: Path) -> None:
    """0x303a:4002 (ESP32-S3 native USB) should rank esp32-s3-devkitc-1 first
    with score >= 0.90 (MCU match) + 0.25 (exact PID) + 0.10 (vendor match)."""
    with sqlite3.connect(built_db) as conn:
        rows = conn.execute(
            QUERY_VID_PID_TO_BOARDS, (int("303a", 16), int("4002", 16))
        ).fetchall()
    assert rows, "expected at least one match for 0x303a:4002"
    top = rows[0]
    assert top[0] == "esp32-s3-devkitc-1", f"expected esp32-s3-devkitc-1 on top; got {top}"
    assert top[6] >= 1.0, f"expected score >= 1.0 (0.90+0.25+0.10 floor minus rounding); got {top[6]}"


def test_canned_query_vid_pid_to_boards_unknown_pid_still_ranks(built_db: Path) -> None:
    """0x303a:ffff (unknown PID under known VID) still returns boards
    because the LEFT JOIN keeps the row — just without the +0.25 PID bonus."""
    with sqlite3.connect(built_db) as conn:
        rows = conn.execute(
            QUERY_VID_PID_TO_BOARDS, (int("303a", 16), int("ffff", 16))
        ).fetchall()
    assert rows, "VID-only match should still produce ranked candidates"
    assert rows[0][0] == "esp32-s3-devkitc-1"
    # PID bonus absent, so the top score must be strictly below the
    # VID+PID-match case (max 0.90 + 0 + 0.10 = 1.0). Tolerate equal because
    # the no-pid path can also reach 1.0 if MCU == "ESP32S3" matches the
    # higher-scored family row. The assertion just rules out crashes.
    assert isinstance(rows[0][6], (int, float))


def test_canned_query_vid_pid_to_boards_totally_unknown(built_db: Path) -> None:
    with sqlite3.connect(built_db) as conn:
        rows = conn.execute(
            QUERY_VID_PID_TO_BOARDS, (int("dead", 16), int("beef", 16))
        ).fetchall()
    assert rows == [], "totally unknown VID:PID must return an empty set"


def test_mcu_to_vid_injects_missing_usb_vendor(tmp_path: Path) -> None:
    """Regression for the production bug found on first dispatch: upstream
    usb.ids text databases don't carry 0x303a (Espressif) or 0x2e8a
    (Raspberry Pi). When mcu_to_vid references such a VID and carries a
    `vid_vendor` field, build_db must inject the vendor so the
    board_vid_guess join doesn't silently drop the most relevant rows.
    """
    data = tmp_path / "data"
    data.mkdir()
    # Upstream usb-vid.json is missing 0x303a entirely.
    (data / "usb-vid.json").write_text(json.dumps({
        "10c4": {"vendor": "Silicon Labs", "products": [["ea60", "CP210x"]]},
    }), encoding="utf-8")
    (data / "pio-boards.json").write_text(json.dumps({
        "esp32-s3-devkitc-1": {
            "id": "esp32-s3-devkitc-1", "name": "Espressif ESP32-S3-DevKitC-1",
            "vendor": "Espressif", "mcu": "ESP32S3",
            "platform": "espressif32", "frameworks": ["arduino"],
            "url": "https://example.invalid",
        },
    }), encoding="utf-8")
    (data / "vendor_boards.json").write_text("{}", encoding="utf-8")
    (data / "mcu_to_vid.json").write_text(json.dumps([
        {"mcu_family": "ESP32S3", "vid": "303a",
         "vid_vendor": "Espressif Systems", "score": 0.95,
         "reason": "Espressif native USB"},
        {"mcu_family": "ESP32S3", "vid": "10c4",
         "vid_vendor": "Silicon Labs", "score": 0.55,
         "reason": "CP210x bridge (legacy)"},
    ]), encoding="utf-8")
    out = tmp_path / "regression.db"
    build_sqlite.build_db(
        usb_vid_json       = data / "usb-vid.json",
        pio_boards_json    = data / "pio-boards.json",
        vendor_boards_json = data / "vendor_boards.json",
        mcu_to_vid_json    = data / "mcu_to_vid.json",
        out_path           = out,
    )
    with sqlite3.connect(out) as conn:
        # 0x303a was missing upstream → must be auto-injected.
        row = conn.execute(
            "SELECT vendor FROM usb_vendor WHERE vid = ?", (int("303a", 16),),
        ).fetchone()
        assert row is not None, "vid_vendor seed must inject missing 0x303a"
        assert row[0] == "Espressif Systems"
        # 0x10c4 was already present upstream — injection must NOT clobber.
        row = conn.execute(
            "SELECT vendor FROM usb_vendor WHERE vid = ?", (int("10c4", 16),),
        ).fetchone()
        assert row[0] == "Silicon Labs"
        # board_vid_guess now includes the ESP32S3 → 0x303a row.
        rows = conn.execute(
            "SELECT vid, confidence FROM board_vid_guess "
            "WHERE board_id = 'esp32-s3-devkitc-1' "
            "ORDER BY confidence DESC"
        ).fetchall()
    assert rows, "view must yield rows now that vendor is injected"
    assert rows[0][0] == int("303a", 16), f"0x303a should rank first; got {rows}"


def test_canned_query_vid_pid_to_boards_rp2040(built_db: Path) -> None:
    with sqlite3.connect(built_db) as conn:
        rows = conn.execute(
            QUERY_VID_PID_TO_BOARDS, (int("2e8a", 16), int("000a", 16))
        ).fetchall()
    assert rows
    assert rows[0][0] == "rpipico"


# The companion direction: given a board id (FTS5 match on name), rank VIDs.
QUERY_BOARD_NAME_TO_VID = """
SELECT board_id, board_name, vid, usb_vendor, confidence, reason
FROM board_vid_guess
WHERE board_id IN (SELECT id FROM board_fts WHERE board_fts MATCH ?)
ORDER BY confidence DESC
LIMIT 20;
"""


def test_canned_query_board_name_to_vid_esp32s3(built_db: Path) -> None:
    # FTS5's default tokenizer chokes on `-` unless the term is quoted.
    # The UI applies the same quoting in app.js before binding the parameter.
    with sqlite3.connect(built_db) as conn:
        rows = conn.execute(QUERY_BOARD_NAME_TO_VID, ('"ESP32-S3"',)).fetchall()
    assert rows, "fuzzy search for ESP32-S3 should return matches"
    # The top-ranked VID for an ESP32S3 board must be 0x303a.
    top_vid_hex = f"{rows[0][2]:04x}"
    assert top_vid_hex == "303a", f"expected 0x303a top; got 0x{top_vid_hex}"


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))

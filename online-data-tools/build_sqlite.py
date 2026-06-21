#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Build the daily SQLite database hosted on the `www` orphan branch.

Reads the four canonical JSON files from the `online-data` branch and emits a
single `<YYYY-MM-DD>.db` containing:

    usb_vendor      (vid, vendor)
    usb_product     (vid, pid, product)
    board           (id, name, vendor, mcu, platform, framework, url)
    mcu_to_vid      (mcu_family, vid, score, reason)
    board_fts       FTS5(id, name, vendor, mcu)  -- contentless view of `board`
    board_vid_guess VIEW joining the above for headline ranking queries

The DB is meant to be served as a static asset and queried client-side via
sql.js (WASM). See issue FastLED/fbuild#718 for the design.

Usage:
    build_sqlite.py \\
      --usb-vid       online-data/data/usb-vid.json \\
      --pio-boards    online-data/data/pio-boards.json \\
      --vendor-boards online-data/data/vendor_boards.json \\
      --mcu-to-vid    online-data/data/mcu_to_vid.json \\
      --out           www/2026-06-20.db
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
from pathlib import Path
from typing import Any


def _load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def _ensure_int(v: int | str) -> int:
    if isinstance(v, int):
        return v
    return int(v, 16)


SCHEMA_SQL = """
PRAGMA foreign_keys = OFF;
PRAGMA journal_mode = OFF;
PRAGMA synchronous  = OFF;

CREATE TABLE usb_vendor (
  vid    INTEGER PRIMARY KEY,
  vendor TEXT    NOT NULL
);

CREATE TABLE usb_product (
  vid     INTEGER NOT NULL,
  pid     INTEGER NOT NULL,
  product TEXT    NOT NULL,
  PRIMARY KEY (vid, pid)
);

CREATE TABLE board (
  id        TEXT PRIMARY KEY,
  name      TEXT NOT NULL,
  vendor    TEXT,
  mcu       TEXT,
  platform  TEXT,
  framework TEXT,
  url       TEXT
);

CREATE TABLE mcu_to_vid (
  mcu_family TEXT    NOT NULL,
  vid        INTEGER NOT NULL,
  score      REAL    NOT NULL,
  reason     TEXT,
  PRIMARY KEY (mcu_family, vid)
);

CREATE INDEX idx_board_mcu       ON board (mcu);
CREATE INDEX idx_mcu_to_vid_vid  ON mcu_to_vid (vid);

CREATE VIRTUAL TABLE board_fts
  USING fts5(id, name, vendor, mcu, content='board', content_rowid='rowid');

-- Per-board headline ranking view. Joins boards to their likely USB
-- vendors via mcu_to_vid. The board_id column carries the original id even
-- when the mcu prefix-match expands to multiple families.
--
-- LEFT JOIN on usb_vendor: some real, allocated VIDs are not present in
-- the public usb.ids text databases we mirror (the Rust `usb-ids` crate,
-- linux-usb.org, Fedora hwdata). 0x303a (Espressif) and 0x2e8a (Raspberry
-- Pi) are the prominent examples. We still want those rows to surface so
-- the heuristic answer (e.g. "ESP32-S3 → 0x303a") is visible; the UI
-- renders a missing vendor name as a hyphen.
CREATE VIEW board_vid_guess AS
SELECT
  b.id     AS board_id,
  b.name   AS board_name,
  b.mcu    AS mcu,
  m.vid    AS vid,
  v.vendor AS usb_vendor,
  m.score  AS confidence,
  m.reason AS reason
FROM board b
JOIN mcu_to_vid m
  ON m.mcu_family = b.mcu
  OR b.mcu LIKE m.mcu_family || '%'
LEFT JOIN usb_vendor v
  ON v.vid = m.vid;
"""


def _populate_usb(conn: sqlite3.Connection, usb_vid: dict) -> None:
    vendor_rows = []
    product_rows = []
    for vid_str, payload in usb_vid.items():
        vid = _ensure_int(vid_str)
        vendor_rows.append((vid, payload["vendor"]))
        for pid_entry in payload.get("products", []):
            # The online-data JSON uses [pid_hex, name] pairs; tolerate the
            # alternate dict shape just in case the upstream format drifts.
            if isinstance(pid_entry, (list, tuple)):
                pid_str, name = pid_entry[0], pid_entry[1]
            else:
                pid_str, name = pid_entry["pid"], pid_entry["name"]
            product_rows.append((vid, _ensure_int(pid_str), name))
    conn.executemany(
        "INSERT INTO usb_vendor (vid, vendor) VALUES (?, ?)", vendor_rows
    )
    conn.executemany(
        "INSERT INTO usb_product (vid, pid, product) VALUES (?, ?, ?)",
        product_rows,
    )


def _populate_boards(
    conn: sqlite3.Connection,
    pio_boards: dict,
    vendor_boards: dict,
) -> None:
    """Insert one row per board. pio-boards is preferred (richer); the
    vendor_boards slim view supplies fallback rows for any id missing in
    pio-boards (defensive — they should be a strict subset)."""
    rows = []
    seen: set[str] = set()
    for board_id, payload in pio_boards.items():
        frameworks = payload.get("frameworks") or []
        rows.append((
            board_id,
            payload.get("name") or board_id,
            payload.get("vendor"),
            payload.get("mcu"),
            payload.get("platform"),
            ",".join(frameworks),
            payload.get("url"),
        ))
        seen.add(board_id)
    for board_id, payload in vendor_boards.items():
        if board_id in seen:
            continue
        rows.append((
            board_id,
            payload.get("name") or board_id,
            payload.get("vendor"),
            payload.get("mcu"),
            None, None, None,
        ))
    conn.executemany(
        "INSERT INTO board (id, name, vendor, mcu, platform, framework, url) "
        "VALUES (?, ?, ?, ?, ?, ?, ?)",
        rows,
    )
    # Populate the FTS5 mirror. content='board' wires the rowid through, but
    # external-content FTS still needs us to push the rows ourselves.
    conn.execute(
        "INSERT INTO board_fts (rowid, id, name, vendor, mcu) "
        "SELECT rowid, id, name, vendor, mcu FROM board"
    )


def _populate_mcu_to_vid(conn: sqlite3.Connection, mcu_to_vid: list[dict]) -> None:
    rows = [
        (entry["mcu_family"], _ensure_int(entry["vid"]),
         float(entry["score"]), entry.get("reason"))
        for entry in mcu_to_vid
    ]
    conn.executemany(
        "INSERT INTO mcu_to_vid (mcu_family, vid, score, reason) "
        "VALUES (?, ?, ?, ?)",
        rows,
    )


def build_db(
    *,
    usb_vid_json: Path,
    pio_boards_json: Path,
    vendor_boards_json: Path,
    mcu_to_vid_json: Path,
    out_path: Path,
) -> None:
    """Construct out_path from the four JSON inputs.

    Overwrites out_path if it exists. Raises on malformed input — the caller
    (the GH Actions workflow) is responsible for preserving the previous day's
    DB if this fails.
    """
    if out_path.exists():
        out_path.unlink()
    out_path.parent.mkdir(parents=True, exist_ok=True)

    usb_vid = _load_json(usb_vid_json)
    pio_boards = _load_json(pio_boards_json)
    vendor_boards = _load_json(vendor_boards_json)
    mcu_to_vid = _load_json(mcu_to_vid_json)
    if not isinstance(mcu_to_vid, list):
        raise ValueError(
            f"{mcu_to_vid_json} must contain a JSON array; got {type(mcu_to_vid).__name__}"
        )

    with sqlite3.connect(out_path) as conn:
        conn.executescript(SCHEMA_SQL)
        _populate_usb(conn, usb_vid)
        _populate_boards(conn, pio_boards, vendor_boards)
        _populate_mcu_to_vid(conn, mcu_to_vid)
        conn.commit()
        # Shrink the file — sql.js downloads the whole DB once, every byte
        # counts on the wire.
        conn.execute("VACUUM")


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--usb-vid",       required=True, type=Path)
    p.add_argument("--pio-boards",    required=True, type=Path)
    p.add_argument("--vendor-boards", required=True, type=Path)
    p.add_argument("--mcu-to-vid",    required=True, type=Path)
    p.add_argument("--out",           required=True, type=Path)
    args = p.parse_args()
    build_db(
        usb_vid_json=args.usb_vid,
        pio_boards_json=args.pio_boards,
        vendor_boards_json=args.vendor_boards,
        mcu_to_vid_json=args.mcu_to_vid,
        out_path=args.out,
    )
    size_kb = args.out.stat().st_size / 1024
    print(f"wrote {args.out} ({size_kb:.1f} KB)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

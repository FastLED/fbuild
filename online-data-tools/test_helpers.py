#!/usr/bin/env -S uv run --no-project --with pytest --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest"]
# ///
"""Tests for the small www-side helper scripts.

Covers:
  - rotate_www_dbs.keep_n_newest: only `<YYYY-MM-DD>.db` files are touched;
    sql-wasm.js / index.html / manifest.json are preserved.
  - build_www_manifest.build: discovers DBs, omits previous_db when only one
    DB is present (first run), includes both when ≥2.
  - annotate_online_manifest.annotate: leaves existing dataset entries intact
    and adds `website` + `databases` top-level keys with the right shape.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import annotate_online_manifest  # noqa: E402
import build_www_manifest        # noqa: E402
import rotate_www_dbs            # noqa: E402


# --------------------------------------------------------------------------- #
# rotate_www_dbs
# --------------------------------------------------------------------------- #

def test_rotation_keeps_newest_two(tmp_path: Path) -> None:
    for name in [
        "2026-06-15.db", "2026-06-16.db", "2026-06-17.db",
        "2026-06-18.db", "2026-06-19.db", "2026-06-20.db",
    ]:
        (tmp_path / name).write_bytes(b"db")
    # Non-db assets that must be preserved
    (tmp_path / "index.html").write_text("<html/>", encoding="utf-8")
    (tmp_path / "sql-wasm.wasm").write_bytes(b"\0wasm")
    (tmp_path / "manifest.json").write_text("{}", encoding="utf-8")

    deleted = rotate_www_dbs.keep_n_newest(tmp_path, keep=2)

    survivors = {p.name for p in tmp_path.iterdir()}
    assert survivors == {
        "2026-06-19.db", "2026-06-20.db",
        "index.html", "sql-wasm.wasm", "manifest.json",
    }, survivors
    assert {d.name for d in deleted} == {
        "2026-06-15.db", "2026-06-16.db",
        "2026-06-17.db", "2026-06-18.db",
    }


def test_rotation_keep_one(tmp_path: Path) -> None:
    for name in ["2026-06-19.db", "2026-06-20.db"]:
        (tmp_path / name).write_bytes(b"db")
    rotate_www_dbs.keep_n_newest(tmp_path, keep=1)
    assert {p.name for p in tmp_path.iterdir()} == {"2026-06-20.db"}


def test_rotation_keep_zero_is_rejected(tmp_path: Path) -> None:
    with pytest.raises(ValueError):
        rotate_www_dbs.keep_n_newest(tmp_path, keep=0)


def test_rotation_ignores_unrelated_db_names(tmp_path: Path) -> None:
    # Files that LOOK like .db but don't match the date pattern must survive.
    (tmp_path / "legacy.db").write_bytes(b"x")
    (tmp_path / "2026-06.db").write_bytes(b"x")        # missing day
    (tmp_path / "2026-06-20.db").write_bytes(b"x")
    rotate_www_dbs.keep_n_newest(tmp_path, keep=1)
    assert {p.name for p in tmp_path.iterdir()} == {
        "legacy.db", "2026-06.db", "2026-06-20.db",
    }


# --------------------------------------------------------------------------- #
# build_www_manifest
# --------------------------------------------------------------------------- #

def test_manifest_two_dbs(tmp_path: Path) -> None:
    (tmp_path / "2026-06-19.db").write_bytes(b"x")
    (tmp_path / "2026-06-20.db").write_bytes(b"x")
    m = build_www_manifest.build(tmp_path)
    assert m["current_db"] == "2026-06-20.db"
    assert m["previous_db"] == "2026-06-19.db"
    assert m["engine"] == "sql.js"
    assert m["payload"] == "wasm"
    assert m["format"] == "sqlite-over-http"


def test_manifest_one_db_omits_previous(tmp_path: Path) -> None:
    (tmp_path / "2026-06-20.db").write_bytes(b"x")
    m = build_www_manifest.build(tmp_path)
    assert m["current_db"] == "2026-06-20.db"
    assert "previous_db" not in m


def test_manifest_no_dbs(tmp_path: Path) -> None:
    m = build_www_manifest.build(tmp_path)
    assert "current_db" not in m
    assert "previous_db" not in m
    assert m["engine"] == "sql.js"


# --------------------------------------------------------------------------- #
# annotate_online_manifest
# --------------------------------------------------------------------------- #

def test_annotation_adds_website_and_databases_without_disturbing_datasets() -> None:
    online = {
        "schema_version": "1.2",
        "generated_at": "2026-06-20T04:17:00Z",
        "datasets": {
            "usb-vid": {"url": "https://example/u.json", "entries": 1942},
            "pio-boards": {"url": "https://example/p.json", "entries": 1553},
        },
    }
    www = {
        "schema_version": "1",
        "current_db": "2026-06-20.db",
        "previous_db": "2026-06-19.db",
        "engine": "sql.js",
    }
    out = annotate_online_manifest.annotate(
        online_manifest=online,
        www_manifest=www,
        website_url="https://fastled.github.io/fbuild/",
    )
    # Datasets must be preserved verbatim.
    assert out["datasets"] == online["datasets"]
    assert out["schema_version"] == "1.2"
    # New website block
    w = out["website"]
    assert w["url"] == "https://fastled.github.io/fbuild/"
    assert w["kind"] == "sqlite-over-http"
    assert w["engine"] == "sql.js"
    assert w["payload"] == "wasm"
    # New databases block — URLs must concat correctly with trailing slash handled.
    d = out["databases"]
    assert d["current"]  == "https://fastled.github.io/fbuild/2026-06-20.db"
    assert d["previous"] == "https://fastled.github.io/fbuild/2026-06-19.db"
    assert d["rotation"] == "daily"
    assert d["stability_window_hours"] == 24


def test_annotation_handles_first_run_single_db() -> None:
    online = {"datasets": {}}
    www = {"current_db": "2026-06-20.db"}  # no previous yet
    out = annotate_online_manifest.annotate(
        online_manifest=online,
        www_manifest=www,
        website_url="https://example.invalid/",
    )
    assert out["databases"]["current"] == "https://example.invalid/2026-06-20.db"
    assert "previous" not in out["databases"]


def test_annotation_does_not_mutate_input() -> None:
    online = {"datasets": {"a": {"x": 1}}}
    www = {"current_db": "2026-06-20.db"}
    annotate_online_manifest.annotate(
        online_manifest=online,
        www_manifest=www,
        website_url="https://example.invalid/",
    )
    # No mutation observable on the input dict.
    assert "website" not in online
    assert "databases" not in online


# --------------------------------------------------------------------------- #
# CLI smoke tests (cover the argparse path so a syntax error is caught early)
# --------------------------------------------------------------------------- #

def test_build_www_manifest_cli_writes_file(tmp_path: Path) -> None:
    (tmp_path / "2026-06-20.db").write_bytes(b"x")
    out = tmp_path / "manifest.json"
    sys.argv = ["build_www_manifest.py", "--www-dir", str(tmp_path), "--out", str(out)]
    rc = build_www_manifest.main()
    assert rc == 0
    data = json.loads(out.read_text(encoding="utf-8"))
    assert data["current_db"] == "2026-06-20.db"


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))

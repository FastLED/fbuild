#!/usr/bin/env -S uv run --no-project --with pytest --with zstandard --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest", "zstandard"]
# ///
"""Tests for build_vendor_archive: tar.zst packaging of the flat vendor map.

The archive shape is a contract: `fbuild-core` will `include_bytes!` the
output, decompress with the `zstd` crate, untar with `tar`, and parse
`usb-vendors.json`. Any drift here breaks fbuild's USB-name lookup.
"""

from __future__ import annotations

import io
import json
import sys
import tarfile
from pathlib import Path

import pytest
import zstandard as zstd

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import build_vendor_archive  # noqa: E402


SAMPLE_USB_VID = {
    "303a": {"vendor": "Espressif Systems", "products": [["4002", "ESP32-S3"]]},
    "10c4": {"vendor": "Silicon Labs",      "products": [["ea60", "CP210x"]]},
    "0403": {"vendor": "FTDI",              "products": [["6001", "FT232"]]},
    "dead": {"vendor": "",                  "products": []},      # blank → skipped
    "BEEF": {"vendor": "Mixed Case In",     "products": []},      # key lowered
}


def _extract(blob: bytes) -> dict[str, bytes]:
    """Decompress + untar `blob` → {filename: file_bytes}."""
    raw = zstd.ZstdDecompressor().decompress(blob)
    out: dict[str, bytes] = {}
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r") as tf:
        for member in tf.getmembers():
            f = tf.extractfile(member)
            assert f is not None
            out[member.name] = f.read()
    return out


def test_flatten_drops_blank_vendor_and_lowercases_keys() -> None:
    flat = build_vendor_archive.flatten_vendors(SAMPLE_USB_VID)
    assert flat == {
        "0403": "FTDI",
        "10c4": "Silicon Labs",
        "303a": "Espressif Systems",
        "beef": "Mixed Case In",
    }


def test_pack_compact_round_trip() -> None:
    """Round-trip the compact format through pack + parse for tricky inputs."""
    tricky = {
        "0001": "plain ascii",
        "0002": "comma, in, the, name",   # commas must be escaped
        "0003": "percent 100% off",       # literal % must be escaped
        "0004": "both 50%, off",          # both
        "0005": "unicode emdash — and é", # arbitrary unicode passes through
        "0006": "",                        # empty value preserved
        "0007": "trailing %25 literal",   # literal "%25" in input must survive
    }
    packed = build_vendor_archive.pack_compact(tricky)
    # No bare comma can appear inside a name field.
    for chunk in packed.split(","):
        vid, sep, name = chunk.partition(":")
        assert sep == ":", f"chunk missing colon: {chunk!r}"
        assert "," not in name, f"raw comma leaked into name: {chunk!r}"
    recovered = build_vendor_archive.parse_compact(packed)
    assert recovered == tricky


def test_pack_compact_handles_empty() -> None:
    assert build_vendor_archive.pack_compact({}) == ""
    assert build_vendor_archive.parse_compact("") == {}


def test_archive_round_trip_decompress() -> None:
    flat = build_vendor_archive.flatten_vendors(SAMPLE_USB_VID)
    blob = build_vendor_archive.build_archive(
        vendors=flat, generated_at="2026-06-21T00:00:00Z",
    )
    files = _extract(blob)
    # Two well-known files inside.
    assert set(files) == {"usb-vendors.txt", "manifest.json"}
    # Compact payload round-trips through parse_compact.
    recovered = build_vendor_archive.parse_compact(
        files["usb-vendors.txt"].decode("utf-8")
    )
    assert recovered == flat
    # Manifest carries the contract metadata.
    manifest = json.loads(files["manifest.json"])
    assert manifest["schema_version"] == build_vendor_archive.SCHEMA_VERSION
    assert manifest["entries"] == len(flat)
    assert manifest["generated_at"] == "2026-06-21T00:00:00Z"
    assert manifest["filename"] == "usb-vendors.txt"
    assert manifest["format"] == "compact-csv-v1"


def test_archive_is_deterministic_for_same_input() -> None:
    """zstd is deterministic given identical input; tarfile gets mtime=0 to
    match. Same vendors + same timestamp => byte-identical archive (so git
    sees no diff on no-op nightly runs)."""
    flat = build_vendor_archive.flatten_vendors(SAMPLE_USB_VID)
    a = build_vendor_archive.build_archive(vendors=flat, generated_at="X")
    b = build_vendor_archive.build_archive(vendors=flat, generated_at="X")
    assert a == b


def test_main_emits_file(tmp_path: Path) -> None:
    src = tmp_path / "usb-vid.json"
    src.write_text(json.dumps(SAMPLE_USB_VID), encoding="utf-8")
    out = tmp_path / "vendors.tar.zst"
    sys.argv = [
        "build_vendor_archive.py",
        "--upstream", str(src),
        "--out", str(out),
        "--generated-at", "2026-06-21T00:00:00Z",
    ]
    rc = build_vendor_archive.main()
    assert rc == 0
    assert out.stat().st_size > 0
    files = _extract(out.read_bytes())
    flat = build_vendor_archive.parse_compact(files["usb-vendors.txt"].decode("utf-8"))
    assert "303a" in flat and flat["303a"] == "Espressif Systems"


def test_main_rejects_empty_input(tmp_path: Path) -> None:
    src = tmp_path / "empty.json"
    src.write_text(json.dumps({"dead": {"vendor": "", "products": []}}),
                   encoding="utf-8")
    out = tmp_path / "x.tar.zst"
    sys.argv = ["build_vendor_archive.py",
                "--upstream", str(src), "--out", str(out)]
    rc = build_vendor_archive.main()
    assert rc == 2  # refuse to write an empty archive


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))

#!/usr/bin/env -S uv run --no-project --with pytest --with zstandard --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pytest", "zstandard"]
# ///
"""Tests for the usb-vid.json -> usb-vids.proto.zstd runtime artifact."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import zstandard as zstd

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import build_usb_vid_proto  # noqa: E402


SAMPLE_USB_VID = {
    "303a": {
        "vendor": "Espressif Systems",
        "products": [["1001", "USB JTAG/serial debug unit"], ["0002", "ESP32-S2"]],
    },
    "10C4": {
        "vendor": "Silicon Labs",
        "products": {"EA60": "CP210x UART Bridge"},
    },
    "dead": {"vendor": "", "products": [["beef", "skipped blank vendor"]]},
    "beef": {"vendor": "No Products Inc", "products": []},
    "cafe": {"vendor": "Invalid Product Inc", "products": [["zzzz", "bad pid"]]},
}


def _read_varint(raw: bytes, pos: int) -> tuple[int, int]:
    shift = 0
    value = 0
    while True:
        byte = raw[pos]
        pos += 1
        value |= (byte & 0x7F) << shift
        if byte < 0x80:
            return value, pos
        shift += 7


def _read_len(raw: bytes, pos: int) -> tuple[bytes, int]:
    size, pos = _read_varint(raw, pos)
    end = pos + size
    return raw[pos:end], end


def _skip(raw: bytes, pos: int, wire_type: int) -> int:
    if wire_type == 0:
        _, pos = _read_varint(raw, pos)
        return pos
    if wire_type == 2:
        _, pos = _read_len(raw, pos)
        return pos
    raise AssertionError(f"unsupported wire type {wire_type}")


def _decode_product(raw: bytes) -> tuple[int, str]:
    pos = 0
    pid = -1
    name = ""
    while pos < len(raw):
        key, pos = _read_varint(raw, pos)
        field = key >> 3
        wire = key & 0x07
        if field == 1 and wire == 0:
            pid, pos = _read_varint(raw, pos)
        elif field == 2 and wire == 2:
            value, pos = _read_len(raw, pos)
            name = value.decode("utf-8")
        else:
            pos = _skip(raw, pos, wire)
    return pid, name


def _decode_vendor(raw: bytes) -> tuple[int, str, list[tuple[int, str]]]:
    pos = 0
    vid = -1
    name = ""
    products: list[tuple[int, str]] = []
    while pos < len(raw):
        key, pos = _read_varint(raw, pos)
        field = key >> 3
        wire = key & 0x07
        if field == 1 and wire == 0:
            vid, pos = _read_varint(raw, pos)
        elif field == 2 and wire == 2:
            value, pos = _read_len(raw, pos)
            name = value.decode("utf-8")
        elif field == 3 and wire == 2:
            value, pos = _read_len(raw, pos)
            products.append(_decode_product(value))
        else:
            pos = _skip(raw, pos, wire)
    return vid, name, products


def _decode_database(raw: bytes) -> list[tuple[int, str, list[tuple[int, str]]]]:
    pos = 0
    vendors: list[tuple[int, str, list[tuple[int, str]]]] = []
    while pos < len(raw):
        key, pos = _read_varint(raw, pos)
        field = key >> 3
        wire = key & 0x07
        if field == 1 and wire == 2:
            value, pos = _read_len(raw, pos)
            vendors.append(_decode_vendor(value))
        else:
            pos = _skip(raw, pos, wire)
    return vendors


def test_encode_database_matches_runtime_schema() -> None:
    proto, stats = build_usb_vid_proto.encode_database(SAMPLE_USB_VID)

    assert stats == build_usb_vid_proto.BuildStats(vendors=2, products=3)
    assert _decode_database(proto) == [
        (0x10C4, "Silicon Labs", [(0xEA60, "CP210x UART Bridge")]),
        (
            0x303A,
            "Espressif Systems",
            [(0x0002, "ESP32-S2"), (0x1001, "USB JTAG/serial debug unit")],
        ),
    ]


def test_compressed_proto_round_trip() -> None:
    proto, _ = build_usb_vid_proto.encode_database(SAMPLE_USB_VID)
    blob = build_usb_vid_proto.compress_proto(proto)
    raw = zstd.ZstdDecompressor().decompress(blob)
    assert _decode_database(raw)[0][1] == "Silicon Labs"


def test_main_emits_proto_zstd(tmp_path: Path) -> None:
    src = tmp_path / "usb-vid.json"
    src.write_text(json.dumps(SAMPLE_USB_VID), encoding="utf-8")
    out = tmp_path / "usb-vids.proto.zstd"
    sys.argv = [
        "build_usb_vid_proto.py",
        "--upstream",
        str(src),
        "--out",
        str(out),
        "--min-products",
        "1",
    ]
    assert build_usb_vid_proto.main() == 0
    assert _decode_database(zstd.ZstdDecompressor().decompress(out.read_bytes()))


def test_main_rejects_too_few_products(tmp_path: Path) -> None:
    src = tmp_path / "usb-vid.json"
    src.write_text(json.dumps(SAMPLE_USB_VID), encoding="utf-8")
    out = tmp_path / "usb-vids.proto.zstd"
    sys.argv = [
        "build_usb_vid_proto.py",
        "--upstream",
        str(src),
        "--out",
        str(out),
        "--min-products",
        "4",
    ]
    assert build_usb_vid_proto.main() == 3
    assert not out.exists()


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))

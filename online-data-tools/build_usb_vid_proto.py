#!/usr/bin/env -S uv run --no-project --with zstandard --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["zstandard"]
# ///
"""Build the compact USB VID:PID protobuf overlay consumed by fbuild.

Input is the merged `usb-vid.json` dataset:

    {
      "303a": {
        "vendor": "Espressif Systems",
        "products": [["1001", "USB JTAG/serial debug unit"]]
      }
    }

Output is `usb-vids.proto.zstd`, a zstd-compressed protobuf using the
wire schema in `crates/fbuild-core/src/usb/data.rs`. The encoder is kept
small and explicit so the online-data workflow can publish the runtime
artifact without needing a generated protobuf toolchain.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Mapping, Sequence

import zstandard as zstd


MIN_PRODUCTS = 1000
PROTO_ZSTD_FILENAME = "usb-vids.proto.zstd"


@dataclass(frozen=True)
class BuildStats:
    vendors: int
    products: int


def _varint(value: int) -> bytes:
    if value < 0:
        raise ValueError("protobuf varint cannot encode negative values")
    out = bytearray()
    while value >= 0x80:
        out.append((value & 0x7F) | 0x80)
        value >>= 7
    out.append(value)
    return bytes(out)


def _key(field_number: int, wire_type: int) -> bytes:
    return _varint((field_number << 3) | wire_type)


def _field_varint(field_number: int, value: int) -> bytes:
    return _key(field_number, 0) + _varint(value)


def _field_bytes(field_number: int, value: bytes) -> bytes:
    return _key(field_number, 2) + _varint(len(value)) + value


def _field_string(field_number: int, value: str) -> bytes:
    return _field_bytes(field_number, value.encode("utf-8"))


def _encode_product(pid: int, name: str) -> bytes:
    return _field_varint(1, pid) + _field_string(2, name)


def _encode_vendor(vid: int, name: str, products: Sequence[tuple[int, str]]) -> bytes:
    out = bytearray()
    out.extend(_field_varint(1, vid))
    out.extend(_field_string(2, name))
    for pid, product_name in products:
        out.extend(_field_bytes(3, _encode_product(pid, product_name)))
    return bytes(out)


_HEX_U16 = re.compile(r"^(?:0x)?([0-9a-fA-F]{1,4})$")


def parse_u16_hex(value: object) -> int | None:
    match = _HEX_U16.match(str(value).strip())
    if not match:
        return None
    return int(match.group(1), 16)


def _product_rows(raw_products: object) -> Iterable[tuple[object, object]]:
    if isinstance(raw_products, Mapping):
        return raw_products.items()
    if not isinstance(raw_products, list):
        return ()
    rows: list[tuple[object, object]] = []
    for row in raw_products:
        if isinstance(row, (list, tuple)) and len(row) == 2:
            rows.append((row[0], row[1]))
    return rows


def normalize_usb_vid(raw: object) -> list[tuple[int, str, list[tuple[int, str]]]]:
    """Return sorted `(vid, vendor, [(pid, product), ...])` rows."""
    if not isinstance(raw, Mapping):
        raise ValueError("usb-vid.json top-level value must be an object")

    vendors: list[tuple[int, str, list[tuple[int, str]]]] = []
    for vid_raw, entry in raw.items():
        vid = parse_u16_hex(vid_raw)
        if vid is None or not isinstance(entry, Mapping):
            continue

        vendor = str(entry.get("vendor", "")).strip()
        if not vendor:
            continue

        products_by_pid: dict[int, str] = {}
        for pid_raw, product_raw in _product_rows(entry.get("products", [])):
            pid = parse_u16_hex(pid_raw)
            product = str(product_raw).strip()
            if pid is None or not product:
                continue
            products_by_pid[pid] = product

        if products_by_pid:
            vendors.append((vid, vendor, sorted(products_by_pid.items())))

    vendors.sort(key=lambda row: row[0])
    return vendors


def encode_database(raw: object) -> tuple[bytes, BuildStats]:
    vendors = normalize_usb_vid(raw)
    out = bytearray()
    product_count = 0
    for vid, vendor, products in vendors:
        product_count += len(products)
        out.extend(_field_bytes(1, _encode_vendor(vid, vendor, products)))
    return bytes(out), BuildStats(vendors=len(vendors), products=product_count)


def compress_proto(raw_proto: bytes, *, level: int = 19) -> bytes:
    return zstd.ZstdCompressor(level=level).compress(raw_proto)


def load_json(path: Path) -> object:
    return json.loads(path.read_text(encoding="utf-8"))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", required=True, type=Path, help="Merged usb-vid.json")
    parser.add_argument(
        "--out",
        required=True,
        type=Path,
        help=f"Output path, usually data/{PROTO_ZSTD_FILENAME}",
    )
    parser.add_argument(
        "--min-products",
        type=int,
        default=MIN_PRODUCTS,
        help="Refuse to write fewer product rows than this floor.",
    )
    parser.add_argument(
        "--compression-level",
        type=int,
        default=19,
        help="zstd compression level.",
    )
    args = parser.parse_args()

    try:
        proto, stats = encode_database(load_json(args.upstream))
    except (OSError, json.JSONDecodeError, ValueError) as e:
        print(f"error: failed to read {args.upstream}: {e}", file=sys.stderr)
        return 2

    if stats.products < args.min_products:
        print(
            f"error: encoded only {stats.products} product rows "
            f"(< floor of {args.min_products}); refusing to write",
            file=sys.stderr,
        )
        return 3

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes(compress_proto(proto, level=args.compression_level))
    print(
        f"wrote {args.out} ({stats.vendors} vendors, {stats.products} products)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

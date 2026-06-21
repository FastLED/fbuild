#!/usr/bin/env -S uv run --no-project --with zstandard --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["zstandard"]
# ///
"""Package the merged USB-vendor catalog as `usb-vendors.tar.zst`.

The archive contains a single flat-shape JSON file:

    usb-vendors.json:
        {"303a": "Espressif Systems", "0483": "STMicroelectronics", ...}

This is what `fbuild` embeds at compile time (via `include_bytes!`) to
resolve a USB VID to its vendor name without depending on the `usb-ids`
Rust crate. PID-level resolution is deliberately NOT in the archive —
clients go to the www-branch SQLite-over-HTTP for that.

Input: a merged `usb-vid.json` in the per-VID schema used elsewhere in
this repo (`{vid: {"vendor": str, "products": [...]}}`); we drop the
products list when emitting the archive.

Compression: zstd level 19 (high ratio, still fast to decompress in Rust
via the `zstd` crate). The whole thing should be a few KB after
compression — call it "build-time embeddable" without bloating the binary.
"""

from __future__ import annotations

import argparse
import io
import json
import re
import sys
import tarfile
from pathlib import Path

try:
    import zstandard as zstd
except ImportError as e:  # pragma: no cover
    raise SystemExit(f"zstandard missing — uv should auto-install. {e}")


# Bumped whenever the embedded archive shape changes — fbuild reads this
# alongside the data so it can refuse to load an incompatible blob.
SCHEMA_VERSION = 2

# In-archive payload format (v2):
#
#   usb-vendors.txt:  "vid:name,vid:name,vid:name,..."
#
# Where `vid` is the 4-hex-digit VID (lowercase) and `name` is the vendor
# name with `,` and `%` percent-escaped (RFC 3986 style, two upper-hex
# digits). Compact for embedding and "inflate on first use" in the Rust
# consumer — see build_archive() for the round-trip invariant and
# parse_compact() below for the reference inflater.
_ESCAPE_RE = re.compile(r"[%,]")
_UNESCAPE_RE = re.compile(r"%([0-9A-Fa-f]{2})")


def _esc(s: str) -> str:
    return _ESCAPE_RE.sub(lambda m: f"%{ord(m.group(0)):02X}", s)


def _unesc(s: str) -> str:
    return _UNESCAPE_RE.sub(lambda m: chr(int(m.group(1), 16)), s)


def flatten_vendors(usb_vid: dict) -> dict[str, str]:
    """{vid: {"vendor": str, "products": [...]}}  →  {vid: vendor}.

    Skips entries whose vendor name is missing / blank so the consumer
    never has to special-case an empty value.
    """
    out: dict[str, str] = {}
    for vid, entry in usb_vid.items():
        if not isinstance(entry, dict):
            continue
        v = entry.get("vendor")
        if not isinstance(v, str) or not v.strip():
            continue
        out[vid.lower()] = v.strip()
    return dict(sorted(out.items()))


def pack_compact(vendors: dict[str, str]) -> str:
    """{vid: name} -> 'vid:name,vid:name,...' with %-escaped name fields.

    Round-trip invariant: parse_compact(pack_compact(v)) == v for any v
    where keys are lowercase 4-hex VIDs and values are arbitrary unicode
    strings (commas and percent signs are safely escaped).
    """
    return ",".join(
        f"{vid}:{_esc(name)}" for vid, name in sorted(vendors.items())
    )


def parse_compact(s: str) -> dict[str, str]:
    """Reference inflater (also used to assert the round-trip in tests).

    The Rust side (`fbuild-core::usb_vendor_db`) implements the same
    parser — keep these two in lock-step on any format change.
    """
    out: dict[str, str] = {}
    if not s:
        return out
    for chunk in s.split(","):
        if not chunk:
            continue
        vid, sep, name_esc = chunk.partition(":")
        if not sep:
            continue
        out[vid] = _unesc(name_esc)
    return out


def build_archive(*, vendors: dict[str, str], generated_at: str) -> bytes:
    """Return the raw bytes of `usb-vendors.tar.zst`.

    The tar contains:
      - usb-vendors.txt   (compact `vid:name,vid:name,...` per pack_compact)
      - manifest.json     (schema_version + generated_at + entry count)
    """
    payload = pack_compact(vendors).encode("utf-8")
    manifest = json.dumps({
        "schema_version": SCHEMA_VERSION,
        "generated_at":   generated_at,
        "entries":        len(vendors),
        "filename":       "usb-vendors.txt",
        "format":         "compact-csv-v1",
        "format_doc":     (
            "ASCII: 'vid:name,vid:name,...'. `vid` is 4-hex-digit lowercase. "
            "`name` is %-escaped per RFC-3986 (chars ',' and '%' only)."
        ),
    }, ensure_ascii=False).encode("utf-8")

    tar_buf = io.BytesIO()
    with tarfile.open(fileobj=tar_buf, mode="w") as tf:
        for name, blob in (("usb-vendors.txt", payload),
                           ("manifest.json", manifest)):
            info = tarfile.TarInfo(name=name)
            info.size = len(blob)
            info.mtime = 0  # deterministic — byte-identical archive for unchanged input
            tf.addfile(info, io.BytesIO(blob))
    raw = tar_buf.getvalue()

    cctx = zstd.ZstdCompressor(level=19)
    return cctx.compress(raw)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", required=True, type=Path,
                   help="Merged usb-vid.json (per-VID schema with products).")
    p.add_argument("--out",      required=True, type=Path,
                   help="Output `.tar.zst` path. Overwritten if present.")
    p.add_argument("--generated-at",
                   help="UTC timestamp embedded in manifest.json. Defaults to now.")
    args = p.parse_args()

    if not args.upstream.is_file():
        print(f"error: {args.upstream} not found", file=sys.stderr)
        return 2

    upstream = json.loads(args.upstream.read_text(encoding="utf-8"))
    vendors = flatten_vendors(upstream)
    if not vendors:
        print(f"error: no vendor entries found in {args.upstream}", file=sys.stderr)
        return 2

    import datetime as _dt
    ts = args.generated_at or _dt.datetime.now(_dt.timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )

    blob = build_archive(vendors=vendors, generated_at=ts)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes(blob)
    print(
        f"wrote {args.out}: {len(vendors)} vendors, "
        f"{len(blob)} bytes (zstd 19, schema={SCHEMA_VERSION})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Union-merge several USB VID:PID name databases into one sorted JSON.

Inputs are heterogeneous:
  - One or more pre-parsed JSON maps (e.g. the dump produced by
    `crates/fbuild-core/examples/dump_usb_ids.rs`). Schema:
        {"VVVV:PPPP": {"vendor": "...", "product": "..."}}
  - One or more raw `usb.ids` text files in linux-usb.org format:
        # comment
        VVVV  Vendor Name
        \tPPPP  Product Name
        \tPPPP  Other Product

Output (written to --out-dir):
  - usb-vid.json            : sorted union, one winner per VID:PID, lowercase keys
  - usb-vid-conflicts.json  : every key where >1 source disagreed (winner + losers)
  - manifest.json           : future-forward dataset index

Source priority (winner on conflict): the order sources were given on the
command line. Convention used by the nightly workflow:
    --json usb-ids-rs.json --txt linux-usb.txt --txt usbids-github.txt
i.e. tier-1 (the bundled Rust crate dump) beats tier-2 (live linux-usb.org)
beats tier-3 (the GitHub mirror).

Fault tolerance:
  - Any input file that doesn't exist or fails to parse is skipped with a
    warning. The script still emits the merged output from whatever sources
    DID load.
  - If the merged usb-vid map has fewer than MIN_ENTRIES entries (default
    1000) we refuse to write the output files — exit non-zero so the nightly
    workflow knows to preserve the previously committed copy. This stops a
    truncated-download incident from blowing away the good data.
  - Existing usb-vid.json files in --out-dir are NOT touched when this
    script declines to emit; the workflow's git step does the preservation.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import re
import sys
from collections import OrderedDict
from pathlib import Path

# A merged dataset with this few entries is almost certainly the result of
# a broken upstream fetch (real DBs have ~16k–24k entries). Refuse to write.
MIN_ENTRIES = 1000

# Constants describing the public surface that consumers read from the
# `online-data` branch. The URLs are baked into the manifest so the
# fbuild app can `GET` the manifest and discover the actual file URL
# without hard-coding the data filename.
DEFAULT_REPO_BRANCH_URL = (
    "https://raw.githubusercontent.com/fastled/fbuild/online-data"
)


def load_json_map(path: Path) -> dict[str, dict[str, str]]:
    """Load a `{VID:PID -> {vendor, product}}` JSON map. Returns {} on failure."""
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError as e:
        print(f"warning: {path}: read failed: {e}", file=sys.stderr)
        return {}
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as e:
        print(f"warning: {path}: JSON parse failed: {e}", file=sys.stderr)
        return {}
    if not isinstance(data, dict):
        print(f"warning: {path}: top-level is not an object", file=sys.stderr)
        return {}
    out: dict[str, dict[str, str]] = {}
    for key, value in data.items():
        norm = _normalize_key(key)
        if norm is None:
            continue
        if (
            not isinstance(value, dict)
            or "vendor" not in value
            or "product" not in value
        ):
            continue
        out[norm] = {
            "vendor": str(value["vendor"]),
            "product": str(value["product"]),
        }
    return out


_VENDOR_LINE = re.compile(r"^([0-9A-Fa-f]{4})\s+(.*?)\s*$")
_PRODUCT_LINE = re.compile(r"^\t([0-9A-Fa-f]{4})\s+(.*?)\s*$")


def parse_usbids_text(path: Path) -> dict[str, dict[str, str]]:
    """Parse a linux-usb.org `usb.ids` text file. Returns {} on failure.

    The file is laid out as:
        VVVV  Vendor Name
        \tPPPP  Product Name
    plus comment lines starting with `#` and class/HID/etc sections at the
    bottom that we don't currently care about. We stop following the
    vendor list at the first non-vendor section header.
    """
    try:
        raw = path.read_text(encoding="utf-8", errors="replace")
    except OSError as e:
        print(f"warning: {path}: read failed: {e}", file=sys.stderr)
        return {}

    out: dict[str, dict[str, str]] = {}
    current_vid: str | None = None
    current_vendor: str | None = None

    for line in raw.splitlines():
        if not line or line.startswith("#"):
            continue
        # End of the vendor section. linux-usb.org puts class tables,
        # HID descriptors, etc. after vendors with a line like
        # "C 01  Audio" or "AT  Audio-Video Transport ...".
        if line[:2] in {"C ", "AT", "HU", "HC", "L ", "HI"} and not line.startswith("\t"):
            break
        m = _VENDOR_LINE.match(line)
        if m:
            current_vid = m.group(1).lower()
            current_vendor = m.group(2)
            continue
        m = _PRODUCT_LINE.match(line)
        if m and current_vid is not None and current_vendor is not None:
            pid = m.group(1).lower()
            product = m.group(2)
            key = f"{current_vid}:{pid}"
            out[key] = {"vendor": current_vendor, "product": product}

    return out


def _normalize_key(key: str) -> str | None:
    """`VVVV:PPPP` (lowercase) or None if the key is malformed."""
    parts = key.strip().split(":")
    if len(parts) != 2:
        return None
    vid, pid = parts
    if len(vid) != 4 or len(pid) != 4:
        return None
    try:
        int(vid, 16)
        int(pid, 16)
    except ValueError:
        return None
    return f"{vid.lower()}:{pid.lower()}"


def merge(sources: list[tuple[str, dict[str, dict[str, str]]]]):
    """Union-merge, recording conflicts. Winner is the first source listed."""
    merged: dict[str, dict[str, str]] = {}
    winner_source: dict[str, str] = {}
    conflicts: dict[str, list[dict[str, str]]] = {}

    for source_name, entries in sources:
        for key, value in entries.items():
            if key not in merged:
                merged[key] = value
                winner_source[key] = source_name
                continue
            existing = merged[key]
            if existing == value:
                continue
            # Real disagreement. Record both sides if this is the first
            # conflict for this key; otherwise append the new losing side.
            if key not in conflicts:
                conflicts[key] = [
                    {
                        "source": winner_source[key],
                        "vendor": existing["vendor"],
                        "product": existing["product"],
                    }
                ]
            conflicts[key].append(
                {
                    "source": source_name,
                    "vendor": value["vendor"],
                    "product": value["product"],
                }
            )

    return merged, conflicts


def write_sorted_json(path: Path, data: dict) -> None:
    """Write a JSON object with sorted keys and a trailing newline."""
    sorted_obj = OrderedDict(sorted(data.items()))
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as f:
        json.dump(sorted_obj, f, indent=2, ensure_ascii=False, sort_keys=True)
        f.write("\n")


def regroup_by_vid(flat: dict[str, dict[str, str]]) -> dict[str, dict]:
    """Convert flat `{vvvv:pppp -> {vendor, product}}` into the nested
    per-VID shape published in `usb-vid.json`:

        {
          "vvvv": {
            "vendor": "Future Technology Devices International, Ltd",
            "products": [
              ["pppp", "FT232 Serial (UART) IC"],
              ["pppp", "FT2232C/D/H Dual UART/FIFO IC"],
              ...
            ]
          },
          ...
        }

    `products` is a list of `[pid, product_name]` two-element arrays
    (tuples in JSON), sorted by pid for stable diffs. The flat shape
    stays the internal representation for merge + conflict tracking
    (1:1 with how the upstream `usb.ids` text indexes things); this
    regrouping happens once at write-time. Per VID the canonical
    vendor name is the first one we encountered (consistent with our
    "first source listed wins" priority) — text-source vendors are
    consistent within a VID by construction, and cross-source vendor-name
    disagreements within a single VID are rare and already captured in
    `usb-vid-conflicts.json` per (vid, pid).
    """
    intermediate: dict[str, tuple[str, dict[str, str]]] = {}
    for flat_key, entry in flat.items():
        vid, pid = flat_key.split(":", 1)
        if vid not in intermediate:
            intermediate[vid] = (entry["vendor"], {})
        intermediate[vid][1][pid] = entry["product"]

    out: dict[str, dict] = {}
    for vid, (vendor, pid_to_name) in intermediate.items():
        out[vid] = {
            "vendor": vendor,
            "products": [[pid, name] for pid, name in sorted(pid_to_name.items())],
        }
    return out


def build_fragment(
    *,
    sources: list[dict[str, str]],
    branch_base_url: str,
) -> dict:
    """Per-dataset manifest fragment merged into `manifest.json` by
    `tools/build_manifest.py`. The discovery script doesn't carry
    per-dataset knowledge anymore (it just lists `data/*.json`); this
    fragment is how a merger contributes the description, key format,
    sources list, and any auxiliary URLs."""
    return {
        "description": (
            "USB vendor catalog — keyed by 4-hex-digit VID, each entry "
            "carries the vendor name and a `products` map of PID → product "
            "name. Union of multiple usb.ids sources, alphabetically sorted, "
            "lowercase hex keys throughout."
        ),
        "key_format": "vvvv (top-level); products: pppp -> product_name",
        "schema": {
            "<vid>": {
                "vendor": "<vendor name>",
                "products": {"<pid>": "<product name>"},
            }
        },
        "conflicts_url": f"{branch_base_url.rstrip('/')}/data/usb-vid-conflicts.json",
        "generated_at": _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "sources": sources,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--json",
        action="append",
        default=[],
        metavar="NAME=PATH",
        help="A pre-parsed JSON source: `--json usb-ids-rs=/tmp/dump.json`. May repeat.",
    )
    parser.add_argument(
        "--txt",
        action="append",
        default=[],
        metavar="NAME=PATH",
        help="A raw `usb.ids` text source: `--txt linux-usb.org=/tmp/usb.ids`. May repeat.",
    )
    parser.add_argument(
        "--out-dir",
        required=True,
        type=Path,
        help="Directory to write usb-vid.json + usb-vid-conflicts.json.",
    )
    parser.add_argument(
        "--manifest-fragment",
        type=Path,
        help=(
            "Optional path to write the per-dataset manifest fragment "
            "(description, sources, conflicts_url, generated_at). "
            "Consumed by tools/build_manifest.py."
        ),
    )
    parser.add_argument(
        "--branch-base-url",
        default=DEFAULT_REPO_BRANCH_URL,
        help=(
            "Base URL of the online-data branch. Embedded in the manifest "
            "fragment (conflicts_url etc.). Defaults to fastled/fbuild."
        ),
    )
    parser.add_argument(
        "--min-entries",
        type=int,
        default=MIN_ENTRIES,
        help=(
            "Sanity floor — refuse to emit if fewer entries survive merging. "
            "Lets the workflow preserve the previously-committed dataset on "
            "broken / truncated upstream fetches."
        ),
    )
    args = parser.parse_args()

    sources_for_manifest: list[dict[str, str]] = []
    ordered_sources: list[tuple[str, dict[str, dict[str, str]]]] = []

    for spec in args.json:
        name, _, raw_path = spec.partition("=")
        if not name or not raw_path:
            print(f"error: --json expects NAME=PATH, got {spec!r}", file=sys.stderr)
            return 2
        path = Path(raw_path)
        entries = load_json_map(path)
        ordered_sources.append((name, entries))
        sources_for_manifest.append(
            {"name": name, "kind": "json", "entries": str(len(entries))}
        )

    for spec in args.txt:
        name, _, raw_path = spec.partition("=")
        if not name or not raw_path:
            print(f"error: --txt expects NAME=PATH, got {spec!r}", file=sys.stderr)
            return 2
        path = Path(raw_path)
        entries = parse_usbids_text(path)
        ordered_sources.append((name, entries))
        sources_for_manifest.append(
            {"name": name, "kind": "usb.ids-text", "entries": str(len(entries))}
        )

    if not ordered_sources:
        print("error: at least one --json or --txt source is required", file=sys.stderr)
        return 2

    merged, conflicts = merge(ordered_sources)

    print(
        f"merged: {len(merged)} entries, {len(conflicts)} conflicts, "
        f"from {len(ordered_sources)} sources",
        file=sys.stderr,
    )

    if len(merged) < args.min_entries:
        print(
            f"error: merged set has only {len(merged)} entries "
            f"(< floor of {args.min_entries}); refusing to write. "
            "The nightly workflow will keep the previously committed data.",
            file=sys.stderr,
        )
        return 3

    out_dir: Path = args.out_dir
    write_sorted_json(out_dir / "usb-vid.json", regroup_by_vid(merged))
    write_sorted_json(out_dir / "usb-vid-conflicts.json", conflicts)

    if args.manifest_fragment is not None:
        fragment = build_fragment(
            sources=sources_for_manifest,
            branch_base_url=args.branch_base_url,
        )
        args.manifest_fragment.parent.mkdir(parents=True, exist_ok=True)
        args.manifest_fragment.write_text(
            json.dumps(fragment, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        print(
            f"wrote: {out_dir / 'usb-vid.json'}, {out_dir / 'usb-vid-conflicts.json'}, "
            f"{args.manifest_fragment}",
            file=sys.stderr,
        )
    else:
        print(
            f"wrote: {out_dir / 'usb-vid.json'}, {out_dir / 'usb-vid-conflicts.json'} "
            "(no --manifest-fragment requested)",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())

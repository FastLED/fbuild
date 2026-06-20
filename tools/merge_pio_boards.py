#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Merge a fresh PlatformIO board dump with the previously committed one.

The contract:
  - The **new** dump is authoritative for any field it carries — if
    PlatformIO renamed or refined something upstream, we want that.
  - Fields that exist only in the **old** dump are preserved on the
    same board. This protects against transient regressions where
    `pio boards` temporarily drops a field (frameworks list narrows,
    `connectivity` blanked, etc.) — we keep what we knew last time.
  - Boards that exist only in the old dump are **not** preserved.
    A board genuinely removed upstream should disappear; otherwise
    stale board ids accumulate forever.
  - We refuse to write if the merged map drops below MIN_BOARDS
    (default 1500); preserves the previously-committed dump if the
    new fetch was truncated/broken.

Usage:
    merge_pio_boards.py --new NEW.json --old OLD.json --out OUT.json
                        [--min-entries N]

  - `--old` may point at a non-existent file (first-ever run): merger
    will then just use `--new` verbatim.
  - `--new` is required and must be a parseable JSON object map.

Exit codes:
  0 — wrote merged JSON.
  2 — bad arguments.
  3 — merged set too small; refusing to write. Caller (workflow) must
      preserve the existing committed file.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import sys
from collections import OrderedDict
from pathlib import Path

MIN_BOARDS_DEFAULT = 1500


def load_map(path: Path) -> dict[str, dict]:
    if not path.is_file():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        print(f"warning: {path}: parse failed: {e}", file=sys.stderr)
        return {}
    if not isinstance(data, dict):
        print(
            f"warning: {path}: top-level is not an object",
            file=sys.stderr,
        )
        return {}
    return {k: v for k, v in data.items() if isinstance(v, dict)}


def deep_merge_board(old: dict, new: dict) -> dict:
    """Union of fields; the new dump wins on overlap.

    For nested objects (e.g. `debug.tools`), we recursively union one
    more level: PlatformIO's `debug.tools` map can grow/shrink as
    debug-probe support changes upstream; we don't want to drop a tool
    entry just because today's dump didn't list it. For everything
    else (lists, scalars), the new value replaces the old wholesale —
    we don't want to merge `frameworks: ["arduino"]` with
    `frameworks: ["arduino", "cmsis"]` and end up with a stale entry.
    """
    merged: dict = {}
    keys = set(old) | set(new)
    for k in keys:
        if k in new and k in old:
            old_v = old[k]
            new_v = new[k]
            if isinstance(old_v, dict) and isinstance(new_v, dict):
                merged[k] = deep_merge_board(old_v, new_v)
            else:
                merged[k] = new_v
        elif k in new:
            merged[k] = new[k]
        else:
            # Only-in-old field — keep it, the new dump dropped it.
            merged[k] = old[k]
    return merged


def merge_boards(
    new: dict[str, dict], old: dict[str, dict]
) -> tuple[dict[str, dict], dict[str, int]]:
    """Per-board union. Boards only in `old` are NOT preserved (see module docstring)."""
    out: dict[str, dict] = {}
    stats = {
        "new_total": len(new),
        "old_total": len(old),
        "common": 0,
        "added": 0,
        "dropped": 0,
        "field_preserved": 0,
    }
    for board_id, new_board in new.items():
        if board_id in old:
            stats["common"] += 1
            merged = deep_merge_board(old[board_id], new_board)
            preserved = sum(1 for k in old[board_id] if k not in new_board)
            stats["field_preserved"] += preserved
            out[board_id] = merged
        else:
            stats["added"] += 1
            out[board_id] = new_board
    stats["dropped"] = len(old) - stats["common"]
    return out, stats


def write_sorted_json(path: Path, data: dict) -> None:
    sorted_obj = OrderedDict(sorted(data.items()))
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(sorted_obj, indent=2, ensure_ascii=False, sort_keys=True) + "\n",
        encoding="utf-8",
    )


# Fields kept in the slim `vendor_boards.json` view. Deliberately tight —
# this is the "what board is plugged in?" lookup, not the full catalog.
# `mcu` is the closest thing PlatformIO ships to a "variant" identifier.
SLIM_FIELDS = ("vendor", "name", "mcu")


def slim_view(full: dict[str, dict]) -> dict[str, dict]:
    """Project the full board catalog down to vendor + name + mcu.

    Boards missing all three fields are dropped — they'd be useless for
    "humanize this board id" lookups. Boards with one or two of the
    three present still get included; we never invent values."""
    slim: dict[str, dict] = {}
    for board_id, full_entry in full.items():
        small: dict[str, str] = {}
        for field in SLIM_FIELDS:
            value = full_entry.get(field)
            if isinstance(value, str) and value:
                small[field] = value
        if small:
            slim[board_id] = small
    return slim


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--new", required=True, type=Path)
    p.add_argument("--old", required=True, type=Path)
    p.add_argument("--out", required=True, type=Path)
    p.add_argument(
        "--out-slim",
        type=Path,
        help=(
            "Optional path for the slim {vendor, name, mcu} view of the "
            "merged catalog. Useful as a small `vendor_boards.json` "
            "lookup when only the human-readable identity is needed."
        ),
    )
    p.add_argument("--min-entries", type=int, default=MIN_BOARDS_DEFAULT)
    p.add_argument(
        "--manifest-fragment",
        type=Path,
        help=(
            "Optional path to write the FULL-catalog manifest fragment "
            "(description, sources, generated_at) for tools/build_manifest.py."
        ),
    )
    p.add_argument(
        "--manifest-fragment-slim",
        type=Path,
        help=(
            "Optional path to write the SLIM (vendor_boards) manifest fragment. "
            "Only meaningful when --out-slim is also given."
        ),
    )
    args = p.parse_args()

    if not args.new.is_file():
        print(f"error: --new {args.new} does not exist", file=sys.stderr)
        return 2

    new_map = load_map(args.new)
    old_map = load_map(args.old)
    merged, stats = merge_boards(new_map, old_map)

    print(
        f"merge_pio_boards: new={stats['new_total']} old={stats['old_total']} "
        f"merged={len(merged)} common={stats['common']} added={stats['added']} "
        f"dropped={stats['dropped']} fields_preserved={stats['field_preserved']}",
        file=sys.stderr,
    )

    if len(merged) < args.min_entries:
        print(
            f"error: merged board count {len(merged)} < floor {args.min_entries}; "
            "refusing to write. The workflow will keep the previously committed file.",
            file=sys.stderr,
        )
        return 3

    write_sorted_json(args.out, merged)
    print(f"wrote {len(merged)} boards to {args.out}", file=sys.stderr)

    slim_count = 0
    if args.out_slim is not None:
        slim = slim_view(merged)
        slim_count = len(slim)
        write_sorted_json(args.out_slim, slim)
        print(
            f"wrote {slim_count} slim {{vendor,name,mcu}} entries to {args.out_slim}",
            file=sys.stderr,
        )
        if args.manifest_fragment_slim is not None:
            slim_fragment = {
                "description": (
                    "Slim view of pio-boards keyed by board id — only "
                    "{vendor, name, mcu} per board. Use this when only "
                    "the human-readable identity is needed (\"what board "
                    "is plugged in?\")."
                ),
                "key_format": "board-id",
                "generated_at": _dt.datetime.now(_dt.timezone.utc).strftime(
                    "%Y-%m-%dT%H:%M:%SZ"
                ),
                "sources": [
                    {
                        "name": "platformio",
                        "kind": "pio-boards-slim",
                        "entries": str(slim_count),
                    }
                ],
            }
            args.manifest_fragment_slim.parent.mkdir(parents=True, exist_ok=True)
            args.manifest_fragment_slim.write_text(
                json.dumps(slim_fragment, indent=2, ensure_ascii=False) + "\n",
                encoding="utf-8",
            )
            print(
                f"wrote slim manifest fragment to {args.manifest_fragment_slim}",
                file=sys.stderr,
            )

    if args.manifest_fragment is not None:
        fragment = {
            "description": (
                "PlatformIO board catalog — every board the PlatformIO "
                "registry knows about, keyed by board id, alphabetically "
                "sorted. Fields the previous dump carried are preserved "
                "across regressions in the live `pio boards` output."
            ),
            "key_format": "board-id",
            "generated_at": _dt.datetime.now(_dt.timezone.utc).strftime(
                "%Y-%m-%dT%H:%M:%SZ"
            ),
            "sources": [
                {
                    "name": "platformio",
                    "kind": "pio-boards",
                    "entries": str(stats["new_total"]),
                }
            ],
            "merge_stats": {
                "common": stats["common"],
                "added": stats["added"],
                "dropped": stats["dropped"],
                "fields_preserved": stats["field_preserved"],
            },
        }
        args.manifest_fragment.parent.mkdir(parents=True, exist_ok=True)
        args.manifest_fragment.write_text(
            json.dumps(fragment, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        print(f"wrote manifest fragment to {args.manifest_fragment}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

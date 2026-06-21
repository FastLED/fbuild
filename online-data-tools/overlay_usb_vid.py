#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Overlay a supplementary `usb-vid.json` (e.g. from `fetch_gowdy_supplement`)
onto the upstream `usb-vid.json` produced by `merge_sources.py`.

Upstream wins on conflict — the supplement is treated as a gap-filler, not
a replacement. Concretely:

  - If a VID is missing from upstream entirely, the supplement's vendor +
    products list is inserted verbatim.
  - If a VID exists in both, the upstream entry is kept untouched (we do
    NOT merge product lists, to keep this script's behavior trivially
    auditable — supplements are for missing-VID gaps, not name patches).

The script writes the result back to the upstream path in place.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def overlay(upstream: dict, supplement: dict) -> tuple[dict, int]:
    """Return (merged_dict, added_vid_count). Upstream is NOT mutated."""
    out = dict(upstream)
    added = 0
    for vid, entry in supplement.items():
        if vid in out:
            continue
        out[vid] = entry
        added += 1
    # Sort by VID so the JSON diff stays stable across runs.
    return dict(sorted(out.items())), added


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream",   required=True, type=Path)
    p.add_argument("--supplement", required=True, type=Path)
    p.add_argument("--out",        required=True, type=Path)
    args = p.parse_args()

    upstream = json.loads(args.upstream.read_text(encoding="utf-8"))
    if not args.supplement.exists():
        print(f"no supplement at {args.supplement} — nothing to overlay")
        return 0
    supplement = json.loads(args.supplement.read_text(encoding="utf-8"))
    merged, added = overlay(upstream, supplement)
    args.out.write_text(
        json.dumps(merged, indent=2, sort_keys=False) + "\n",
        encoding="utf-8",
    )
    print(
        f"overlaid {args.supplement.name} onto {args.upstream.name}: "
        f"+{added} VID(s), total={len(merged)}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

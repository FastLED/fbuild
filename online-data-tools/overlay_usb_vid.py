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


def overlay(
    upstream: dict, supplement: dict, *, mode: str = "gap-fill"
) -> tuple[dict, int]:
    """Return (merged_dict, changed_vid_count). Upstream is NOT mutated.

    Modes:
      - "gap-fill" (default): supplement is consulted only for VIDs missing
        from upstream. Use when the supplement is a less-authoritative
        gap-filler (live scrape, second-tier source, etc.).
      - "vendor-override": supplement is the HIGHER-authority source for
        vendor names. For any VID present in both, the supplement's vendor
        name replaces upstream's, but the upstream products list is kept
        untouched. For VIDs only in supplement, the entry is added verbatim
        (products usually empty). This is what the workflow uses for the
        curated `vendor_names_inlined.py` supplement.

    `changed_vid_count` is the number of VIDs added OR vendor-renamed.
    """
    if mode not in ("gap-fill", "vendor-override"):
        raise ValueError(f"unknown mode: {mode!r}")
    out = {k: dict(v) for k, v in upstream.items()}  # deep-ish copy
    changed = 0
    for vid, sup_entry in supplement.items():
        if vid not in out:
            out[vid] = dict(sup_entry)
            changed += 1
            continue
        if mode == "gap-fill":
            continue
        # vendor-override: replace name, keep products intact.
        sup_name = sup_entry.get("vendor")
        if sup_name and out[vid].get("vendor") != sup_name:
            out[vid]["vendor"] = sup_name
            changed += 1
    return dict(sorted(out.items())), changed


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream",   required=True, type=Path)
    p.add_argument("--supplement", required=True, type=Path)
    p.add_argument("--out",        required=True, type=Path)
    p.add_argument("--mode", default="gap-fill",
                   choices=("gap-fill", "vendor-override"),
                   help="See overlay() docstring for semantics.")
    args = p.parse_args()

    upstream = json.loads(args.upstream.read_text(encoding="utf-8"))
    if not args.supplement.exists():
        print(f"no supplement at {args.supplement} — nothing to overlay")
        return 0
    supplement = json.loads(args.supplement.read_text(encoding="utf-8"))
    merged, changed = overlay(upstream, supplement, mode=args.mode)
    args.out.write_text(
        json.dumps(merged, indent=2, sort_keys=False) + "\n",
        encoding="utf-8",
    )
    print(
        f"overlaid {args.supplement.name} onto {args.upstream.name} "
        f"(mode={args.mode}): {changed} VID(s) added or renamed, "
        f"total={len(merged)}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

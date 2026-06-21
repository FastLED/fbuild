#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Collapse a VID's name list in ids3.json down to one chosen entry.

    keep.py <vid> <index>

`<vid>` matches the 4-hex-digit key; `<index>` is 0-based into the
existing list at that VID. The chosen name becomes the entry's sole
member (still wrapped in a list so the file shape stays consistent).

Designed for the iterative dedupe loop driven by `next_dual.py`. Both
files write the same indent=2, sort-by-vid JSON shape so diffs stay
minimal between steps.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("vid")
    p.add_argument("index", type=int)
    p.add_argument("--input", default="ids3.json", type=Path)
    args = p.parse_args()

    data = json.loads(args.input.read_text(encoding="utf-8"))
    vid = args.vid.lower()
    if vid not in data:
        print(f"error: vid {vid!r} not in {args.input}", file=sys.stderr)
        return 2
    entry = data[vid]
    if not isinstance(entry, list):
        print(f"error: {vid} entry is not a list ({type(entry).__name__})",
              file=sys.stderr)
        return 2
    if not 0 <= args.index < len(entry):
        print(f"error: index {args.index} out of range for {vid} "
              f"(len={len(entry)})", file=sys.stderr)
        return 2

    kept = entry[args.index]
    data[vid] = [kept]
    args.input.write_text(
        json.dumps(dict(sorted(data.items())), indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    print(f"{vid}: kept [{args.index}] {kept!r}; dropped {len(entry) - 1} other(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

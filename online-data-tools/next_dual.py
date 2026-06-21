#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Print the first VID in a list-shaped IDs JSON that has >=2 candidate
vendor names. Companion to `keep.py` — together they implement the manual
dedupe loop the user asked for:

    next_dual.py            # shows the next multi-entry to triage
    keep.py <vid> <index>   # collapses that entry to a single chosen name
    next_dual.py            # … repeat until empty

Exits 0 with an empty stdout when no multi-entries remain.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--input", default="ids3.json", type=Path)
    args = p.parse_args()
    data = json.loads(args.input.read_text(encoding="utf-8"))
    for vid in sorted(data):
        v = data[vid]
        if isinstance(v, list) and len(v) >= 2:
            payload = {"vid": vid, "names": v}
            print(json.dumps(payload, indent=2, ensure_ascii=False))
            return 0
    # No multi-entries left.
    return 0


if __name__ == "__main__":
    sys.exit(main())

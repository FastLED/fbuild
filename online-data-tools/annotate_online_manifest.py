#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Post-process `online-data/manifest.json` to link out to the `www` branch.

The base `manifest.json` is produced by `online-data/tools/build_manifest.py`
which knows nothing about the new sibling `www` branch. This script adds two
top-level keys without touching anything build_manifest.py already wrote:

    {
      ... existing schema_version, generated_at, datasets ...,
      "website": {
        "url": "https://fastled.github.io/fbuild/",
        "kind": "sqlite-over-http",
        "engine": "sql.js",
        "payload": "wasm",
        "description": "Browser-side fuzzy search over USB VID:PID and PIO boards"
      },
      "databases": {
        "current": "https://fastled.github.io/fbuild/2026-06-20.db",
        "previous": "https://fastled.github.io/fbuild/2026-06-19.db",
        "rotation": "daily",
        "stability_window_hours": 24
      }
    }

The script reads the www manifest produced by `build_www_manifest.py` to
discover the day-rotated filenames so this output stays in sync.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def annotate(
    *,
    online_manifest: dict,
    www_manifest: dict,
    website_url: str,
) -> dict:
    base = website_url.rstrip("/")
    out = dict(online_manifest)  # shallow copy — keep dataset substructures intact
    out["website"] = {
        "url": website_url,
        "kind": "sqlite-over-http",
        "engine": "sql.js",
        "payload": "wasm",
        "description": (
            "Browser-side fuzzy search over USB VID:PID and PIO boards"
        ),
    }
    dbs: dict = {"rotation": "daily", "stability_window_hours": 24}
    if "current_db" in www_manifest:
        dbs["current"] = f"{base}/{www_manifest['current_db']}"
    if "previous_db" in www_manifest:
        dbs["previous"] = f"{base}/{www_manifest['previous_db']}"
    out["databases"] = dbs
    return out


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--online-manifest", required=True, type=Path,
                   help="Path to online-data/manifest.json (mutated in place).")
    p.add_argument("--www-manifest",    required=True, type=Path,
                   help="Path to www/manifest.json (read-only).")
    p.add_argument("--website-url",     required=True,
                   help="Canonical site URL, e.g. https://fastled.github.io/fbuild/")
    args = p.parse_args()

    online = json.loads(args.online_manifest.read_text(encoding="utf-8"))
    www = json.loads(args.www_manifest.read_text(encoding="utf-8"))
    annotated = annotate(
        online_manifest=online,
        www_manifest=www,
        website_url=args.website_url,
    )
    args.online_manifest.write_text(
        json.dumps(annotated, indent=2, sort_keys=False) + "\n",
        encoding="utf-8",
    )
    print(
        f"annotated {args.online_manifest.name}: website={args.website_url}, "
        f"databases={annotated['databases']}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

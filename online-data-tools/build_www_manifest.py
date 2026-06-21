#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Emit `www/manifest.json` advertising the day-rotated SQLite databases.

The www/ directory holds 1–2 `<YYYY-MM-DD>.db` files. This script discovers
them and writes a tiny manifest the front-end's app.js consumes to decide
which file to fetch:

    {
      "schema_version": "1",
      "generated_at": "2026-06-20T04:17:00Z",
      "current_db": "2026-06-20.db",
      "previous_db": "2026-06-19.db",
      "engine": "sql.js",
      "payload": "wasm",
      "format": "sqlite-over-http"
    }

`previous_db` is omitted on the very first run when only one .db exists.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import re
import sys
from pathlib import Path

_DB_NAME = re.compile(r"^\d{4}-\d{2}-\d{2}\.db$")


def discover_dbs(www_dir: Path) -> list[str]:
    return sorted(
        (p.name for p in www_dir.iterdir() if p.is_file() and _DB_NAME.match(p.name)),
        reverse=True,
    )


def build(www_dir: Path) -> dict:
    dbs = discover_dbs(www_dir)
    out: dict = {
        "schema_version": "1",
        "generated_at": _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "engine": "sql.js",
        "payload": "wasm",
        "format": "sqlite-over-http",
    }
    if dbs:
        out["current_db"] = dbs[0]
    if len(dbs) >= 2:
        out["previous_db"] = dbs[1]
    return out


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--www-dir", required=True, type=Path)
    p.add_argument("--out", required=True, type=Path)
    args = p.parse_args()
    manifest = build(args.www_dir)
    args.out.write_text(
        json.dumps(manifest, indent=2, sort_keys=False) + "\n",
        encoding="utf-8",
    )
    print(f"wrote {args.out} ({manifest.get('current_db', '<no db>')})")
    return 0


if __name__ == "__main__":
    sys.exit(main())

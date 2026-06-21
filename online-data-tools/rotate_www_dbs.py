#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Keep only the two most-recent `<YYYY-MM-DD>.db` files in the www/ worktree.

Pattern matched: ``YYYY-MM-DD.db`` (the calendar dates are sortable as
strings). Anything else in the directory — index.html, app.js, sql-wasm.*,
manifest.json — is left untouched.

The two we keep are exposed via ``manifest.json`` as ``current_db`` (newest)
and ``previous_db`` (second newest). The grace window lets a client that
fetched ``manifest.json`` just before a refresh continue to download a
working DB after the refresh swaps the pointer.

Usage:
    rotate_www_dbs.py --www-dir www [--keep 2]
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

_DB_NAME = re.compile(r"^\d{4}-\d{2}-\d{2}\.db$")


def keep_n_newest(www_dir: Path, keep: int) -> list[Path]:
    """Delete all but the `keep` newest `<YYYY-MM-DD>.db` files. Returns the
    deleted paths so the caller can print or log them."""
    if keep < 1:
        raise ValueError(f"--keep must be >= 1, got {keep}")
    dbs = sorted(
        (p for p in www_dir.iterdir() if p.is_file() and _DB_NAME.match(p.name)),
        key=lambda p: p.name,  # ISO-8601 dates sort lexicographically
        reverse=True,
    )
    to_delete = dbs[keep:]
    for p in to_delete:
        p.unlink()
    return to_delete


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--www-dir", required=True, type=Path)
    p.add_argument("--keep", type=int, default=2)
    args = p.parse_args()
    if not args.www_dir.is_dir():
        print(f"error: {args.www_dir} is not a directory", file=sys.stderr)
        return 2
    deleted = keep_n_newest(args.www_dir, args.keep)
    for d in deleted:
        print(f"deleted: {d.name}")
    print(f"kept {args.keep} newest .db file(s) in {args.www_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Assemble `manifest.json` for the `online-data` branch by discovering
every JSON file under `--data-dir`.

The script does **not** carry hardcoded knowledge of which datasets exist —
it just lists what's there. Drop a new `data/foo.json` onto the branch
and the next manifest run automatically exposes a `datasets["foo"]` entry
pointing at it. This is the future-forward path: extending the branch
with a new dataset means adding a new merger that writes a new JSON file
into `data/`; the manifest catches up on its own.

Per-dataset metadata (description, key format, sources list, custom
links such as `conflicts_url`) is supplied by the per-dataset merger as
a `--fragment NAME=PATH` JSON file. The fragment is merged into the
discovered dataset record, so anything the script can't infer from the
filename alone is owned by the merger that produced the data.

Usage:
    build_manifest.py --branch-base-url URL --data-dir DIR --out PATH \\
                      [--fragment NAME=PATH ...]

Always emits a fresh `generated_at` (UTC ISO 8601). Exits 0 on success.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import sys
from pathlib import Path


SCHEMA_VERSION = "1.2"


def now_utc() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def entry_count(path: Path) -> int | None:
    """Cheap entry count for a JSON object/array — returns None if unparseable."""
    if not path.is_file():
        return None
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None
    if isinstance(data, dict):
        return len(data)
    if isinstance(data, list):
        return len(data)
    return None


def load_fragment(spec: str) -> tuple[str, dict]:
    name, _, raw_path = spec.partition("=")
    if not name or not raw_path:
        raise SystemExit(f"--fragment expects NAME=PATH, got {spec!r}")
    path = Path(raw_path)
    if not path.is_file():
        return name, {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        print(f"warning: {path}: fragment parse failed: {e}", file=sys.stderr)
        return name, {}
    if not isinstance(data, dict):
        return name, {}
    return name, data


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--branch-base-url", required=True)
    p.add_argument("--data-dir", required=True, type=Path)
    p.add_argument("--out", required=True, type=Path)
    p.add_argument(
        "--fragment",
        action="append",
        default=[],
        metavar="NAME=PATH",
        help=(
            "Per-dataset metadata fragment (description, key_format, sources, "
            "conflicts_url, ...). NAME matches the dataset key, which equals "
            "the data file's stem (so `--fragment usb-vid=...` decorates the "
            "entry for `data/usb-vid.json`). Repeatable."
        ),
    )
    args = p.parse_args()

    branch_base = args.branch_base_url.rstrip("/")
    fragments = dict(load_fragment(spec) for spec in args.fragment)

    data_dir: Path = args.data_dir
    if not data_dir.is_dir():
        print(f"error: --data-dir {data_dir} does not exist", file=sys.stderr)
        return 2

    datasets_out: dict[str, dict] = {}
    for data_path in sorted(data_dir.glob("*.json")):
        stem = data_path.stem
        count = entry_count(data_path)
        record: dict = {
            "url": f"{branch_base}/data/{data_path.name}",
            "format": "json-object",
            "entries": count if count is not None else 0,
            "status": "ok" if count and count > 0 else "missing",
        }
        # Per-dataset fragment overlays / augments (description,
        # key_format, sources, conflicts_url, ...). The fragment owns
        # all metadata the merger knows about its own dataset.
        if stem in fragments:
            for k, v in fragments[stem].items():
                record[k] = v
        datasets_out[stem] = record

    manifest = {
        "schema_version": SCHEMA_VERSION,
        "generated_at": now_utc(),
        "datasets": datasets_out,
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    print(
        f"wrote manifest with {len(datasets_out)} dataset(s) "
        f"({', '.join(sorted(datasets_out)) or 'none'}) to {args.out}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

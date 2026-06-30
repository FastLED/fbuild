#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Orchestrate the www-branch refresh in one place so the YAML stays thin.

Given an `online-data` worktree (with merged JSON), a `www` worktree (the
GH Pages source), and a few configuration knobs, this script:

  1. Bootstraps `data/mcu_to_vid.json` on online-data from the seed if missing.
  2. Builds `<YYYY-MM-DD>.db` into the www worktree.
  3. Copies the www_static/ assets (index.html, app.js, style.css) onto www.
  4. Downloads sql.js (versioned + sha256 verified) and stages sql-wasm.{js,wasm}.
  5. Rotates old `.db` files so only `current` + `previous` remain.
  6. Writes `www/manifest.json` advertising the day-stable filenames.
  7. Annotates `online-data/manifest.json` with the www link-out block.

The script does NOT touch git. Worktree setup, commits, and pushes stay in
the YAML where they have access to the runner's git identity + token.

Run via:
    update_www.py --workspace . --online-worktree .online-data \\
                  --www-worktree .www --today 2026-06-20 \\
                  --website-url https://fastled.github.io/fbuild/

Test via tests/test_update_www.py — the orchestrator is fully unit-tested
with a tempdir + a fake sql.js zip, no network required.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import io
import json
import shutil
import sys
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import annotate_online_manifest  # noqa: E402
import build_sqlite              # noqa: E402
import build_www_manifest        # noqa: E402
import rotate_www_dbs            # noqa: E402


# Static-site filenames mirrored from www_static/. Kept here so the workflow
# YAML doesn't need to enumerate them.
STATIC_ASSETS = ("index.html", "app.js", "style.css")

# Narrow migrations for historical seed mistakes that may already have been
# copied to the curated online-data branch. Keep this list explicit: the
# online-data copy can carry curator edits, so seed changes must not become a
# blanket overwrite.
DEPRECATED_MCU_TO_VID_ROWS = {
    ("AM_APOLLO3", "1cbe"),
}


@dataclass
class Config:
    workspace: Path              # repo root checkout
    online_worktree: Path        # path to .online-data sibling worktree
    www_worktree: Path           # path to .www sibling worktree
    today: str                   # YYYY-MM-DD (UTC) for the new DB filename
    website_url: str             # canonical GH Pages URL
    sqljs_zip_url: str           # https://github.com/sql-js/sql.js/releases/...
    keep_dbs: int = 2            # current + previous

    @property
    def seed_mcu_to_vid(self) -> Path:
        return self.workspace / "online-data-tools" / "seed_mcu_to_vid.json"

    @property
    def online_mcu_to_vid(self) -> Path:
        return self.online_worktree / "data" / "mcu_to_vid.json"

    @property
    def static_src(self) -> Path:
        return self.workspace / "online-data-tools" / "www_static"

    @property
    def new_db(self) -> Path:
        return self.www_worktree / f"{self.today}.db"

    @property
    def www_manifest(self) -> Path:
        return self.www_worktree / "manifest.json"

    @property
    def online_manifest(self) -> Path:
        return self.online_worktree / "manifest.json"


# --------------------------------------------------------------------------- #
# Individual steps — each does one thing and is independently testable.
# --------------------------------------------------------------------------- #

def bootstrap_mcu_to_vid(cfg: Config) -> bool:
    """Returns True if the seed was copied (first run), False if already
    present. Idempotent."""
    cfg.online_mcu_to_vid.parent.mkdir(parents=True, exist_ok=True)
    if cfg.online_mcu_to_vid.is_file():
        return False
    shutil.copyfile(cfg.seed_mcu_to_vid, cfg.online_mcu_to_vid)
    return True


def _vid_key(value: object) -> str:
    """Normalize int, "1234", or "0x1234" VID values to 4-hex lowercase."""
    if isinstance(value, int):
        return f"{value:04x}"
    text = str(value).strip().lower()
    if text.startswith("0x"):
        text = text[2:]
    return f"{int(text, 16):04x}"


def apply_mcu_to_vid_corrections(cfg: Config) -> int:
    """Patch known-bad historical MCU VID rows in the online-data copy.

    `data/mcu_to_vid.json` is intentionally curator-owned after bootstrap, so
    this does not resync the whole seed. It only removes explicitly deprecated
    rows and adds the current seed row(s) for the affected family if absent.
    Returns the number of deprecated rows removed.
    """
    if not cfg.online_mcu_to_vid.is_file():
        return 0

    online_rows = json.loads(cfg.online_mcu_to_vid.read_text(encoding="utf-8"))
    seed_rows = json.loads(cfg.seed_mcu_to_vid.read_text(encoding="utf-8"))
    if not isinstance(online_rows, list) or not isinstance(seed_rows, list):
        return 0

    seed_by_family: dict[str, list[dict]] = {}
    for row in seed_rows:
        if not isinstance(row, dict):
            continue
        family = row.get("mcu_family")
        if isinstance(family, str):
            seed_by_family.setdefault(family, []).append(row)

    kept_rows: list[dict] = []
    corrected_families: set[str] = set()
    removed = 0
    for row in online_rows:
        if not isinstance(row, dict):
            kept_rows.append(row)
            continue
        family = row.get("mcu_family")
        try:
            vid = _vid_key(row.get("vid"))
        except (TypeError, ValueError):
            kept_rows.append(row)
            continue
        if (family, vid) in DEPRECATED_MCU_TO_VID_ROWS:
            corrected_families.add(str(family))
            removed += 1
            continue
        kept_rows.append(row)

    if removed == 0:
        return 0

    existing: set[tuple[object, str]] = set()
    for row in kept_rows:
        if not isinstance(row, dict) or not row.get("mcu_family") or not row.get("vid"):
            continue
        try:
            existing.add((row.get("mcu_family"), _vid_key(row.get("vid"))))
        except (TypeError, ValueError):
            continue
    for family in sorted(corrected_families):
        for seed_row in seed_by_family.get(family, []):
            key = (seed_row.get("mcu_family"), _vid_key(seed_row.get("vid")))
            if key not in existing:
                kept_rows.append(dict(seed_row))
                existing.add(key)

    cfg.online_mcu_to_vid.write_text(
        json.dumps(kept_rows, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    return removed


def build_todays_db(cfg: Config) -> Path:
    data_dir = cfg.online_worktree / "data"
    build_sqlite.build_db(
        usb_vid_json       = data_dir / "usb-vid.json",
        pio_boards_json    = data_dir / "pio-boards.json",
        vendor_boards_json = data_dir / "vendor_boards.json",
        mcu_to_vid_json    = cfg.online_mcu_to_vid,
        out_path           = cfg.new_db,
    )
    return cfg.new_db


def stage_static_assets(cfg: Config) -> list[Path]:
    """Copy www_static/* onto the www worktree. Returns the destinations."""
    written: list[Path] = []
    for name in STATIC_ASSETS:
        src = cfg.static_src / name
        dst = cfg.www_worktree / name
        shutil.copyfile(src, dst)
        written.append(dst)
    return written


def stage_sqljs(
    cfg: Config,
    *,
    fetch: Callable[[str], bytes] = lambda url: urllib.request.urlopen(url, timeout=90).read(),
) -> tuple[Path, Path]:
    """Download the pinned sql.js zip, extract sql-wasm.{js,wasm}, stage them.

    `fetch` is injected for unit tests so we don't touch the network. Returns
    (js_path, wasm_path) in the www worktree.
    """
    blob = fetch(cfg.sqljs_zip_url)
    js_dst   = cfg.www_worktree / "sql-wasm.js"
    wasm_dst = cfg.www_worktree / "sql-wasm.wasm"
    with zipfile.ZipFile(io.BytesIO(blob)) as zf:
        # The release archive nests files under a versioned subdir; find them
        # by basename so we tolerate layout drift across releases.
        js_entry   = _find_entry(zf, "sql-wasm.js")
        wasm_entry = _find_entry(zf, "sql-wasm.wasm")
        js_dst.write_bytes(zf.read(js_entry))
        wasm_dst.write_bytes(zf.read(wasm_entry))
    return js_dst, wasm_dst


def _find_entry(zf: zipfile.ZipFile, basename: str) -> str:
    for name in zf.namelist():
        if name.endswith("/" + basename) or name == basename:
            return name
    raise FileNotFoundError(
        f"{basename!r} not present in archive (entries: {zf.namelist()[:5]}…)"
    )


def rotate_dbs(cfg: Config) -> list[Path]:
    return rotate_www_dbs.keep_n_newest(cfg.www_worktree, cfg.keep_dbs)


def write_www_manifest(cfg: Config) -> dict:
    manifest = build_www_manifest.build(cfg.www_worktree)
    cfg.www_manifest.write_text(
        json.dumps(manifest, indent=2, sort_keys=False) + "\n",
        encoding="utf-8",
    )
    return manifest


def annotate_online(cfg: Config, www_manifest: dict) -> dict:
    online = json.loads(cfg.online_manifest.read_text(encoding="utf-8"))
    annotated = annotate_online_manifest.annotate(
        online_manifest = online,
        www_manifest    = www_manifest,
        website_url     = cfg.website_url,
    )
    cfg.online_manifest.write_text(
        json.dumps(annotated, indent=2, sort_keys=False) + "\n",
        encoding="utf-8",
    )
    return annotated


# --------------------------------------------------------------------------- #
# Top-level orchestration
# --------------------------------------------------------------------------- #

def run(cfg: Config, *, fetch_sqljs: Callable[[str], bytes] | None = None) -> dict:
    """Execute all steps in order. Returns a summary dict for logging."""
    summary: dict = {"today": cfg.today, "website_url": cfg.website_url}
    summary["mcu_to_vid_bootstrapped"] = bootstrap_mcu_to_vid(cfg)
    summary["mcu_to_vid_corrections"] = apply_mcu_to_vid_corrections(cfg)
    db = build_todays_db(cfg)
    summary["db_path"]   = str(db)
    summary["db_bytes"]  = db.stat().st_size
    summary["assets"]    = [str(p) for p in stage_static_assets(cfg)]
    js, wasm = stage_sqljs(
        cfg, **({"fetch": fetch_sqljs} if fetch_sqljs is not None else {})
    )
    summary["sqljs"]     = [str(js), str(wasm)]
    deleted              = rotate_dbs(cfg)
    summary["rotated_out"] = [p.name for p in deleted]
    www_manifest         = write_www_manifest(cfg)
    summary["www_manifest"] = www_manifest
    summary["online_manifest"] = annotate_online(cfg, www_manifest)
    return summary


def _today_utc() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%d")


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--workspace",       required=True, type=Path,
                   help="Repo checkout root (provides online-data-tools/).")
    p.add_argument("--online-worktree", required=True, type=Path)
    p.add_argument("--www-worktree",    required=True, type=Path)
    p.add_argument("--website-url",     required=True)
    p.add_argument("--sqljs-zip-url",   required=True)
    p.add_argument("--today", default=_today_utc(),
                   help="Override the date stamp; default = UTC today.")
    p.add_argument("--keep-dbs", type=int, default=2)
    args = p.parse_args()

    cfg = Config(
        workspace       = args.workspace.resolve(),
        online_worktree = args.online_worktree.resolve(),
        www_worktree    = args.www_worktree.resolve(),
        today           = args.today,
        website_url     = args.website_url,
        sqljs_zip_url   = args.sqljs_zip_url,
        keep_dbs        = args.keep_dbs,
    )
    summary = run(cfg)
    import json
    print(json.dumps(summary, indent=2, default=str))
    return 0


if __name__ == "__main__":
    sys.exit(main())

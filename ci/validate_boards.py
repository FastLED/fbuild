#!/usr/bin/env python3
"""Validate that fbuild's board JSON assets match PlatformIO's board definitions.

This script compares the enriched board JSONs in
crates/fbuild-config/assets/boards/json/ against PlatformIO's installed
platform board files (~/.platformio/platforms/<platform>/boards/<id>.json).

It extracts the same fields as enrich_boards.rs (build, upload) and reports
any differences. Exits non-zero if any board is out of sync.

Usage:
    python ci/validate_boards.py [--platforms atmelavr,espressif32,...]
    python ci/validate_boards.py --external [--json]

If --platforms is not specified, validates all platforms that are installed.
Use --external to compare against Arduino and Zephyr board registries (requires internet).
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path


# Must match enrich_boards.rs exactly
BUILD_FIELDS = ("core", "variant", "extra_flags", "f_cpu", "f_flash", "f_image", "flash_mode", "mcu")
ARDUINO_FIELDS = ("ldscript", "partitions", "memory_type")
UPLOAD_FIELDS = (
    "protocol",
    "speed",
    "flash_size",
    "require_upload_port",
    "maximum_ram_size",
    "maximum_size",
    "use_1200bps_touch",
    "wait_for_upload_port",
)

MEGATINYCORE_EXTRA_FLAGS = (
    "-DCLOCK_SOURCE=0",
    '-DMEGATINYCORE="2.6.11"',
    "-DMEGATINYCORE_MAJOR=2UL",
    "-DMEGATINYCORE_MINOR=6UL",
    "-DMEGATINYCORE_PATCH=11UL",
    "-DMEGATINYCORE_RELEASED=1",
    "-DCORE_ATTACH_ALL",
    "-DTWI_MORS",
    "-DUSE_TIMERD0_PWM",
)

DXCORE_EXTRA_FLAGS = (
    "-DCLOCK_SOURCE=0",
    '-DDXCORE="1.5.6"',
    "-DDXCORE_MAJOR=1UL",
    "-DDXCORE_MINOR=5UL",
    "-DDXCORE_PATCH=6UL",
    "-DDXCORE_RELEASED=1",
    "-DCORE_ATTACH_ALL",
    "-DTWI_MORS_SINGLE",
    "-DMILLIS_USE_TIMERB2",
)


def home_dir() -> Path:
    home = os.environ.get("USERPROFILE") if sys.platform == "win32" else os.environ.get("HOME")
    return Path(home or "")


def pio_platforms_dir() -> Path:
    return home_dir() / ".platformio" / "platforms"


def assets_boards_dir() -> Path:
    """Locate the fbuild board assets directory relative to this script."""
    return Path(__file__).resolve().parent.parent / "crates" / "fbuild-config" / "assets" / "boards" / "json"


def normalize_extra_flags(val: object) -> str:
    """Normalize extra_flags to a space-separated string (matches enrich_boards.rs)."""
    if isinstance(val, list):
        return " ".join(str(v) for v in val)
    if isinstance(val, str):
        return val
    return ""


def framework_extra_flags(core: str | None) -> tuple[str, ...]:
    if core == "megatinycore":
        return MEGATINYCORE_EXTRA_FLAGS
    if core == "dxcore":
        return DXCORE_EXTRA_FLAGS
    return ()


def merge_extra_flags(core: str | None, flags: str) -> str:
    merged = flags.split()
    existing = set(merged)

    # PlatformIO injects these framework defines during the build rather than
    # storing them in the board JSON, but fbuild relies on the static board
    # assets carrying the full define set.
    for flag in framework_extra_flags(core):
        if flag not in existing:
            merged.append(flag)

    return " ".join(merged)


def extract_build(pio_build: dict) -> dict:
    """Extract relevant build fields from PlatformIO's build section."""
    build: dict = {}
    core = pio_build.get("core")
    for field in BUILD_FIELDS:
        if field in pio_build:
            val = pio_build[field]
            if field == "extra_flags":
                val = merge_extra_flags(core if isinstance(core, str) else None, normalize_extra_flags(val))
            build[field] = val

    # Extract VID/PID from hwids (array of [vid, pid] pairs — take the first)
    hwids = pio_build.get("hwids")
    if isinstance(hwids, list) and hwids:
        first = hwids[0]
        if isinstance(first, list) and len(first) >= 2:
            build["vid"] = first[0]
            build["pid"] = first[1]

    # Extract arduino sub-fields
    if "arduino" in pio_build and isinstance(pio_build["arduino"], dict):
        arduino = {}
        for field in ARDUINO_FIELDS:
            if field in pio_build["arduino"]:
                arduino[field] = pio_build["arduino"][field]
        if arduino:
            build["arduino"] = arduino

    return build


def extract_upload(pio_upload: dict) -> dict:
    """Extract relevant upload fields from PlatformIO's upload section."""
    upload: dict = {}
    for field in UPLOAD_FIELDS:
        if field in pio_upload:
            upload[field] = pio_upload[field]
    return upload


def find_pio_board(board_id: str, platform: str, pio_dir: Path) -> dict | None:
    """Find the full PlatformIO board JSON for a given board_id and platform."""
    # Try the base platform directory
    board_path = pio_dir / platform / "boards" / f"{board_id}.json"
    if board_path.exists():
        return json.loads(board_path.read_text(encoding="utf-8"))

    # Try versioned platform directories (espressif32@src-xxx)
    if pio_dir.exists():
        prefix = f"{platform}@"
        for entry in pio_dir.iterdir():
            if entry.name.startswith(prefix) and entry.is_dir():
                board_path = entry / "boards" / f"{board_id}.json"
                if board_path.exists():
                    return json.loads(board_path.read_text(encoding="utf-8"))

    return None


def diff_dicts(expected: dict, actual: dict, path: str = "") -> list[str]:
    """Compare two dicts and return human-readable differences."""
    diffs: list[str] = []
    all_keys = sorted(set(list(expected.keys()) + list(actual.keys())))
    for key in all_keys:
        full_path = f"{path}.{key}" if path else key
        if key not in expected:
            diffs.append(f"  + {full_path}: {actual[key]!r} (extra in our asset)")
        elif key not in actual:
            diffs.append(f"  - {full_path}: {expected[key]!r} (missing from our asset)")
        elif isinstance(expected[key], dict) and isinstance(actual[key], dict):
            diffs.extend(diff_dicts(expected[key], actual[key], full_path))
        elif expected[key] != actual[key]:
            diffs.append(f"  ~ {full_path}: expected {expected[key]!r}, got {actual[key]!r}")
    return diffs


def validate_board(board_path: Path, pio_dir: Path) -> list[str] | None:
    """Validate a single board JSON against PlatformIO's source.

    Returns a list of difference strings, or None if the board was skipped
    (platform not installed).
    """
    board = json.loads(board_path.read_text(encoding="utf-8"))
    board_id = board.get("id", board_path.stem)
    platform = board.get("platform", "")

    if not platform:
        return None

    pio_board = find_pio_board(board_id, platform, pio_dir)
    if pio_board is None:
        return None  # Platform not installed, skip

    diffs: list[str] = []

    # Compare build section
    pio_build = pio_board.get("build", {})
    if isinstance(pio_build, dict):
        expected_build = extract_build(pio_build)
        actual_build = board.get("build", {})
        if expected_build and expected_build != actual_build:
            diffs.extend(diff_dicts(expected_build, actual_build, "build"))

    # Compare upload section
    pio_upload = pio_board.get("upload", {})
    if isinstance(pio_upload, dict):
        expected_upload = extract_upload(pio_upload)
        actual_upload = board.get("upload", {})
        if expected_upload and expected_upload != actual_upload:
            diffs.extend(diff_dicts(expected_upload, actual_upload, "upload"))

    return diffs


def get_installed_platforms(pio_dir: Path) -> set[str]:
    """Return the set of platform names that are installed."""
    if not pio_dir.exists():
        return set()
    platforms: set[str] = set()
    for entry in pio_dir.iterdir():
        if entry.is_dir():
            name = entry.name.split("@")[0]  # strip version suffix
            if (entry / "boards").is_dir():
                platforms.add(name)
    return platforms


def run_external_comparison(output_json: bool = False) -> int:
    """Compare fbuild boards against Arduino + Zephyr external sources.

    Delegates to ci/board_sources.py --compare.
    """
    from board_sources import (
        compare_boards,
        fetch_all_arduino,
        fetch_zephyr_boards,
        load_fbuild_board_names,
        load_fbuild_boards,
    )

    print("Fetching external board lists...", file=sys.stderr)
    arduino_reports = fetch_all_arduino()
    zephyr_report = fetch_zephyr_boards()
    all_reports = arduino_reports + [zephyr_report]

    fbuild_ids = load_fbuild_boards()
    fbuild_names = load_fbuild_board_names()

    missing = compare_boards(all_reports, fbuild_ids, fbuild_names)

    total_external = sum(len(r.boards) for r in all_reports if not r.error)
    total_missing = sum(len(boards) for boards in missing.values())
    errors = [r for r in all_reports if r.error]

    if output_json:
        out = {
            "fbuild_board_count": len(fbuild_ids),
            "external_board_count": total_external,
            "missing_count": total_missing,
            "errors": [{"source": r.source_id, "error": r.error} for r in errors],
            "missing_by_source": {
                src: [
                    {"name": b.name, "architecture": b.architecture, "vendor": b.vendor}
                    for b in boards
                ]
                for src, boards in sorted(missing.items())
            },
        }
        print(json.dumps(out, indent=2))
    else:
        print(f"\n{'=' * 60}")
        print("External Board Coverage Comparison")
        print(f"{'=' * 60}")
        print(f"  fbuild boards:       {len(fbuild_ids)}")
        print(f"  External boards:     {total_external}")
        print(f"  Missing from fbuild: {total_missing}")

        if errors:
            print(f"\n  Fetch errors ({len(errors)}):")
            for r in errors:
                print(f"    {r.source_id}: {r.error}")

        if missing:
            print(f"\n{'─' * 60}")
            print("Boards in external sources but NOT in fbuild:")
            print(f"{'─' * 60}")
            for src, boards in sorted(missing.items()):
                print(f"\n  [{src}] ({len(boards)} boards):")
                for board in sorted(boards, key=lambda b: b.name)[:50]:
                    parts = [f"    {board.name}"]
                    if board.architecture:
                        parts.append(f"[{board.architecture}]")
                    print(" ".join(parts))
                if len(boards) > 50:
                    print(f"    ... and {len(boards) - 50} more")
        else:
            print("\nAll external boards have matches in fbuild!")

    return 1 if missing else 0


def main() -> int:
    # Parse flags
    filter_platforms: set[str] | None = None
    run_external = False
    output_json = False
    args = sys.argv[1:]
    i = 0
    while i < len(args):
        if args[i] == "--platforms" and i + 1 < len(args):
            filter_platforms = set(args[i + 1].split(","))
            i += 2
        elif args[i] == "--external":
            run_external = True
            i += 1
        elif args[i] == "--json":
            output_json = True
            i += 1
        else:
            print(f"Unknown argument: {args[i]}", file=sys.stderr)
            print(__doc__, file=sys.stderr)
            return 1

    if run_external:
        return run_external_comparison(output_json)

    boards_dir = assets_boards_dir()
    pio_dir = pio_platforms_dir()

    if not boards_dir.exists():
        print(f"Error: board assets not found at {boards_dir}", file=sys.stderr)
        return 1

    if not pio_dir.exists():
        print(f"Error: PlatformIO platforms not found at {pio_dir}", file=sys.stderr)
        print("Install PlatformIO and required platforms first.", file=sys.stderr)
        return 1

    installed = get_installed_platforms(pio_dir)
    if filter_platforms:
        missing = filter_platforms - installed
        if missing:
            print(f"Error: requested platforms not installed: {', '.join(sorted(missing))}", file=sys.stderr)
            return 1
        check_platforms = filter_platforms
    else:
        check_platforms = installed

    print(f"Validating boards for platforms: {', '.join(sorted(check_platforms))}")
    print(f"Board assets directory: {boards_dir}")
    print()

    board_files = sorted(boards_dir.glob("*.json"))
    total = 0
    checked = 0
    skipped = 0
    passed = 0
    failed = 0
    failures: list[tuple[str, list[str]]] = []

    for board_path in board_files:
        total += 1

        # Quick filter: read platform from JSON
        try:
            board = json.loads(board_path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            skipped += 1
            continue

        platform = board.get("platform", "")
        if platform not in check_platforms:
            skipped += 1
            continue

        checked += 1
        diffs = validate_board(board_path, pio_dir)

        if diffs is None:
            skipped += 1
            checked -= 1
            continue

        if diffs:
            failed += 1
            board_id = board.get("id", board_path.stem)
            failures.append((board_id, diffs))
        else:
            passed += 1

    # Report results
    print(f"Results: {total} total, {checked} checked, {passed} passed, {failed} failed, {skipped} skipped")
    print()

    if failures:
        print(f"FAILED: {failed} board(s) have drifted from PlatformIO definitions:")
        print()
        for board_id, diffs in sorted(failures):
            print(f"  {board_id}:")
            for diff in diffs:
                print(f"    {diff}")
            print()
        print("To fix: run 'soldr cargo run -p fbuild-config --bin enrich_boards' and commit the changes.")
        return 1

    print("All checked boards match PlatformIO definitions.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

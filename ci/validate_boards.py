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
from dataclasses import dataclass
from pathlib import Path


# Must match enrich_boards.rs exactly
BUILD_FIELDS = ("core", "variant", "extra_flags", "f_cpu", "f_flash", "f_image", "flash_mode", "mcu")
ARDUINO_FIELDS = ("ldscript", "partitions", "memory_type")

# Intentional fbuild-only extensions to the board schema that PlatformIO does
# not carry upstream. Stripped from the actual side before diffing so they
# aren't reported as drift. Keep this list short and document why each entry
# is here — every addition is a place where fbuild semantically extends PIO.
FBUILD_EXTENSION_BUILD_FIELDS = frozenset(
    {
        # Teensy 3.x/4.x CMSIS-DSP per-MCU library name (FastLED/fbuild#300).
        # PlatformIO leaves this implicit and lets Teensyduino's makefile pick
        # the matching `-l...` flag; fbuild needs it declared so the linker can
        # auto-link the right `arm_cortexM*_math` archive.
        "cmsis_dsp_lib",
    }
)
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

# Board IDs that are intentionally fbuild-native: shipped in
# crates/fbuild-config/assets/boards/json/ but have no upstream PlatformIO
# board JSON to validate against. Without this allowlist they'd silently land
# in the generic "skipped" bucket alongside boards whose platform just isn't
# installed on this host, which masks the distinction between "intentional
# skip" and "platform missing from CI install set."
#
# Reconciles FastLED/FastLED#2845 Stage 3 item 8 ("validate_boards.py
# reconciliation"), option (b): document the skip as intentional for
# non-PlatformIO platforms. See FastLED/FastLED#2836 / fbuild#419 /
# fbuild#420 for the LPC8xx bare-metal target work that introduced these.
FBUILD_NATIVE_BOARDS = frozenset(
    {
        # NXP LPC8xx bare-metal targets. PlatformIO's `nxplpc` platform
        # ships lpc11u68 / lpc1768 / lpc54114 / lpc546xx / lpcxpresso55s16
        # / seeedArchPro, but not these five — they are fbuild-only.
        # The bare MCU ids (`lpc845`, `lpc804`) plus the three Arduino-style
        # board variants ship under the vendored ArduinoCore-LPC8xx framework
        # (FastLED/fbuild#456 item 3.8, #479, #487). Without listing the
        # variants here, validate_boards.py silently skips them via the
        # "platform not installed" code path (find_pio_board → None) even
        # though the platform IS installed — the board just doesn't exist
        # upstream. Listing them surfaces them in the fbuild-native tally
        # and keeps future drift from being silently masked.
        "lpc845",
        "lpc804",
        "lpc845brk",
        "lpcxpresso804",
        "lpcxpresso845max",
        # FastLED/fbuild#740 back-fill: popular community boards that
        # ship no PlatformIO board JSON but have well-known runtime USB
        # VID:PID that fbuild needs to identify. Added to give the
        # tier-1 board-VID:PID supplement first-party rows for these
        # devices; no PlatformIO row exists to diff against.
        "adafruit_qt_py_rp2040",
        "seeed_xiao_rp2040",
        "seeed_xiao_mg24",
        "nice_nano",
        "nrfmicro",
        # Teknic publishes ClearCore through its Arduino Board Manager index,
        # not PlatformIO. fbuild pins that official vendor package directly
        # so the SAME53 compile target can be validated without a fake SAMD51
        # board definition (FastLED/FastLED#3839).
        "clearcore",
    }
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


# Per-board validation outcomes. The three skip-ish cases used to collapse
# into a single `None` return, which is how 61 boards the pinned platform
# packages do not carry were counted as "skipped, platform not installed" and
# the job still printed "All checked boards match".
MATCH = "match"
DRIFT = "drift"
UNMATCHED = "unmatched"  # platform is installed, but it ships no <id>.json
NO_PLATFORM = "no-platform"  # the board asset itself declares no platform

BASELINE_PATH = Path(__file__).resolve().parent / "validate_boards_baseline.json"


@dataclass(frozen=True)
class Validation:
    status: str
    diffs: tuple[str, ...] = ()


def load_baseline(path: Path | None = None) -> dict[str, frozenset[str]]:
    """Board ids known to have no upstream JSON in the platform pin.

    A missing file is an empty baseline (every unmatched board then fails);
    a malformed one is a hard error -- silently ignoring it would turn the
    gate back into "unmatched boards are fine".
    """
    baseline_path = BASELINE_PATH if path is None else path
    if not baseline_path.exists():
        return {}
    doc = json.loads(baseline_path.read_text(encoding="utf-8"))
    boards = doc.get("boards")
    if not isinstance(boards, dict):
        raise ValueError(f"{baseline_path}: 'boards' must map platform -> list of board ids")
    return {str(platform): frozenset(str(i) for i in ids) for platform, ids in boards.items()}


def compare_board(board: dict, pio_board: dict) -> list[str]:
    """Return the differences between our board asset and PlatformIO's dict."""
    diffs: list[str] = []

    # Compare build section. Both sides are diffed even when PlatformIO
    # declares nothing comparable: `if expected_build and ...` used to
    # short-circuit the entire comparison, so an asset carrying build fields
    # the upstream board never declares was reported as a match.
    pio_build = pio_board.get("build", {})
    expected_build = extract_build(pio_build if isinstance(pio_build, dict) else {})
    raw_build = board.get("build", {})
    actual_build = raw_build if isinstance(raw_build, dict) else {}
    # Strip intentional fbuild-only extensions from the actual side so
    # they aren't reported as drift (see FBUILD_EXTENSION_BUILD_FIELDS).
    actual_build = {k: v for k, v in actual_build.items() if k not in FBUILD_EXTENSION_BUILD_FIELDS}
    if expected_build or actual_build:
        diffs.extend(diff_dicts(expected_build, actual_build, "build"))

    # Same for upload.
    pio_upload = pio_board.get("upload", {})
    expected_upload = extract_upload(pio_upload if isinstance(pio_upload, dict) else {})
    raw_upload = board.get("upload", {})
    actual_upload = raw_upload if isinstance(raw_upload, dict) else {}
    if expected_upload or actual_upload:
        diffs.extend(diff_dicts(expected_upload, actual_upload, "upload"))

    return diffs


def validate_board(board_path: Path, pio_dir: Path) -> Validation:
    """Compare one board JSON against PlatformIO's source."""
    board = json.loads(board_path.read_text(encoding="utf-8"))
    board_id = board.get("id", board_path.stem)
    platform = board.get("platform", "")

    if not platform:
        return Validation(NO_PLATFORM)

    pio_board = find_pio_board(board_id, platform, pio_dir)
    if pio_board is None:
        return Validation(UNMATCHED)

    diffs = compare_board(board, pio_board)
    return Validation(DRIFT if diffs else MATCH, tuple(diffs))


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
            "missing_by_source": {src: [{"name": b.name, "architecture": b.architecture, "vendor": b.vendor} for b in boards] for src, boards in sorted(missing.items())},
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


def main() -> int:  # noqa: C901
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

    baseline = load_baseline()
    board_files = sorted(boards_dir.glob("*.json"))
    total = 0
    compared = 0  # boards a real field-by-field comparison ran on
    unavailable = 0  # platform not installed on this host
    unreadable = 0
    fbuild_native = 0
    fbuild_native_ids: list[str] = []
    passed = 0
    drifted = 0
    failed = 0
    failures: list[tuple[str, list[str]]] = []
    unmatched_known: list[tuple[str, str]] = []
    unmatched_new: list[tuple[str, str]] = []

    for board_path in board_files:
        total += 1

        # Quick filter: read platform from JSON
        try:
            board = json.loads(board_path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            unreadable += 1
            continue

        board_id = board.get("id", board_path.stem)

        # Boards with no upstream PlatformIO equivalent are tallied separately
        # so the summary line distinguishes them from generic "platform not
        # installed on this host" skips. See FBUILD_NATIVE_BOARDS rationale.
        if board_id in FBUILD_NATIVE_BOARDS:
            fbuild_native += 1
            fbuild_native_ids.append(board_id)
            continue

        platform = board.get("platform", "")
        if not platform:
            failed += 1
            failures.append((board_id, ["  ! board asset declares no 'platform'"]))
            continue
        if platform not in check_platforms:
            unavailable += 1
            continue

        outcome = validate_board(board_path, pio_dir)

        if outcome.status == NO_PLATFORM:
            failed += 1
            failures.append((board_id, ["  ! board asset declares no 'platform'"]))
            continue

        if outcome.status == UNMATCHED:
            # The platform IS installed but ships no JSON for this id. Not a
            # silent skip: every such board must be declared in the baseline,
            # so a new board that cannot be validated fails the gate instead of
            # quietly joining the "skipped" bucket.
            if board_id in baseline.get(platform, frozenset()):
                unmatched_known.append((platform, board_id))
            else:
                unmatched_new.append((platform, board_id))
                failed += 1
                failures.append(
                    (
                        board_id,
                        [
                            f"  ! no upstream {board_id}.json in the installed {platform} platform: "
                            "nothing was compared. If this board is intentionally unvalidatable, "
                            "add it to ci/validate_boards_baseline.json."
                        ],
                    )
                )
            continue

        compared += 1
        if outcome.status == DRIFT:
            drifted += 1
            failed += 1
            failures.append((board_id, list(outcome.diffs)))
        else:
            passed += 1

    # A run that compared nothing verifies nothing -- the whole point of the
    # gate. Before this check an install step that produced no platforms (or a
    # board set nothing matched) still printed "All checked boards match" rc 0.
    if compared == 0 and not drifted:
        print(
            f"Error: {total} board asset(s) found but none could be validated "
            f"({len(check_platforms)} platform(s) selected, {unavailable} board(s) skipped as "
            "platform-not-installed). Nothing was verified.",
            file=sys.stderr,
        )
        return 1

    # Report results
    print(
        f"Results: {total} total, {compared} compared, {passed} passed, {drifted} drifted, "
        f"{failed} failed, {unavailable} platform-not-installed, {fbuild_native} fbuild-native, "
        f"{len(unmatched_known)} known-unmatched, {len(unmatched_new)} NEW-unmatched"
    )
    if unreadable:
        print(f"  unreadable board JSON (not validated): {unreadable}")
    if unmatched_known:
        by_platform: dict[str, int] = {}
        for platform, _board_id in unmatched_known:
            by_platform[platform] = by_platform.get(platform, 0) + 1
        detail = ", ".join(f"{p}={n}" for p, n in sorted(by_platform.items()))
        print(f"  known-unmatched (no upstream JSON in the platform pin -- see ci/validate_boards_baseline.json): {detail}")
    if fbuild_native:
        ids_str = ", ".join(sorted(fbuild_native_ids))
        print(f"  fbuild-native (intentional skip, no PlatformIO upstream — see FastLED/FastLED#2845): {ids_str}")
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

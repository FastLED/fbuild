#!/usr/bin/env python3
"""Extract build flags from PlatformIO for all platforms and write reference JSONs.

This script extracts the authoritative compiler flags, preprocessor defines,
and linker flags that PlatformIO uses for each board/MCU. The output is written
to per-platform reference directories under crates/fbuild-build/.

For platforms with fbuild modules (teensy, esp32, avr):
    crates/fbuild-build/src/<platform>/configs/reference/<board>.json

For platforms without modules yet (rp, stm32, wasm):
    crates/fbuild-build/reference/<platform>/<board>.json

These reference JSONs are ``include_str!``'d in Rust tests to validate that
fbuild's MCU configs and ``BoardConfig::get_defines()`` stay in sync with
PlatformIO.

Usage:
    # Extract all boards across all platforms:
    uv run python ci/extract_pio_build_flags.py --all

    # Extract a specific platform:
    uv run python ci/extract_pio_build_flags.py --platform teensy
    uv run python ci/extract_pio_build_flags.py --platform esp

    # Extract specific boards:
    uv run python ci/extract_pio_build_flags.py --board teensy36 --board esp32

    # Validate only (compare existing references against fbuild configs):
    uv run python ci/extract_pio_build_flags.py --validate
    uv run python ci/extract_pio_build_flags.py --validate --platform teensy
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PLATFORM_CONFIGS_ROOT = REPO_ROOT / "build" / "lib" / "fbuild" / "platform_configs"
FBUILD_BUILD_SRC = REPO_ROOT / "crates" / "fbuild-build" / "src"
FBUILD_BUILD_REF = REPO_ROOT / "crates" / "fbuild-build" / "reference"
TEST_PROJECTS_DIR = REPO_ROOT / "tests"

# ── Platform registry ────────────────────────────────────────────────────────
# Maps platform_config directory names to their fbuild-build module names
# (or None if no module exists yet).  Also maps board IDs to MCU config
# family names for validation.

PLATFORMS: dict[str, dict] = {
    "teensy": {
        "module": "teensy",
        "board_to_mcu_config": {
            "teensy36": "teensy3x",
            "teensy35": "teensy3x",
            "teensy31": "teensy3x",
            "teensy30": "teensy3x",
            "teensy41": "teensy4x",
            "teensy40": "teensy4x",
            "teensylc": "teensylc",
        },
    },
    "esp": {
        "module": "esp32",
        "board_to_mcu_config": {
            "esp32": "esp32",
            "esp32c2": "esp32c2",
            "esp32c3": "esp32c3",
            "esp32c5": "esp32c5",
            "esp32c6": "esp32c6",
            "esp32h2": "esp32h2",
            "esp32p4": "esp32p4",
            "esp32s2": "esp32s2",
            "esp32s3": "esp32s3",
            "esp8266": None,  # no MCU config yet
        },
    },
    "avr": {
        "module": "avr",
        "board_to_mcu_config": {
            "avr": "avr",
        },
    },
    "rp": {
        "module": None,  # no fbuild-build module yet
        "board_to_mcu_config": {},
    },
    "stm32": {
        "module": None,
        "board_to_mcu_config": {},
    },
    "wasm": {
        "module": None,
        "board_to_mcu_config": {},
    },
}


def get_platform_for_board(board: str) -> str | None:
    """Find which platform a board belongs to by checking platform_configs dirs."""
    for platform_dir in sorted(PLATFORM_CONFIGS_ROOT.iterdir()):
        if not platform_dir.is_dir():
            continue
        if (platform_dir / f"{board}.json").exists():
            return platform_dir.name
    return None


def get_reference_dir(platform: str) -> Path:
    """Get the reference directory for a platform."""
    info = PLATFORMS.get(platform, {})
    module = info.get("module")
    if module:
        return FBUILD_BUILD_SRC / module / "configs" / "reference"
    return FBUILD_BUILD_REF / platform


def get_mcu_configs_dir(platform: str) -> Path | None:
    """Get the MCU configs directory for a platform (if it has a module)."""
    info = PLATFORMS.get(platform, {})
    module = info.get("module")
    if module:
        return FBUILD_BUILD_SRC / module / "configs"
    return None


def find_all_boards() -> list[tuple[str, str]]:
    """Find all (platform, board) pairs from platform_configs."""
    results = []
    for platform_dir in sorted(PLATFORM_CONFIGS_ROOT.iterdir()):
        if not platform_dir.is_dir():
            continue
        platform = platform_dir.name
        for path in sorted(platform_dir.glob("*.json")):
            results.append((platform, path.stem))
    return results


def find_boards_for_platform(platform: str) -> list[tuple[str, str]]:
    """Find all boards for a specific platform."""
    config_dir = PLATFORM_CONFIGS_ROOT / platform
    if not config_dir.exists():
        return []
    return [(platform, p.stem) for p in sorted(config_dir.glob("*.json"))]


def normalize_defines(raw_defines: list) -> dict[str, str]:
    """Normalize PIO's mixed define format into a flat {name: value} dict.

    PIO stores defines as either:
    - ``"FOO"`` → implies value ``"1"``
    - ``["FOO", "val"]`` → explicit key-value pair
    - ``"FOO=val"`` → inline key=value (some configs use this)
    """
    result: dict[str, str] = {}
    for entry in raw_defines:
        if isinstance(entry, list) and len(entry) == 2:
            result[entry[0]] = str(entry[1])
        elif isinstance(entry, str):
            if "=" in entry:
                key, _, val = entry.partition("=")
                result[key] = val
            else:
                result[entry] = "1"
    return result


def extract_from_platform_configs(platform: str, board: str) -> dict | None:
    """Extract all build flags from the Python platform_configs."""
    config_path = PLATFORM_CONFIGS_ROOT / platform / f"{board}.json"
    if not config_path.exists():
        return None
    data = json.loads(config_path.read_text(encoding="utf-8"))

    result: dict = {
        "board": board,
        "mcu": data.get("mcu", ""),
        "platform": platform,
        "source": f"PlatformIO {platform} platform (extracted from platform_configs)",
    }

    # Compiler flags
    compiler_flags = data.get("compiler_flags", {})
    if compiler_flags:
        result["compiler_flags"] = {
            "common": compiler_flags.get("common", []),
            "c": compiler_flags.get("c", []),
            "cxx": compiler_flags.get("cxx", []),
        }

    # Defines
    raw_defines = data.get("defines", [])
    if raw_defines:
        result["defines"] = normalize_defines(raw_defines)

    # Linker flags
    result["linker_flags"] = data.get("linker_flags", [])
    result["linker_libs"] = data.get("linker_libs", [])

    return result


def extract_from_pio_envdump(platform: str, board: str) -> dict | None:
    """Extract build flags by running PlatformIO's environment dump."""
    test_dir = TEST_PROJECTS_DIR / board
    if not test_dir.exists():
        return None

    try:
        result = subprocess.run(
            ["pio", "project", "metadata", "--json-output", "-e", board],
            cwd=str(test_dir),
            capture_output=True,
            text=True,
            timeout=120,
        )
        if result.returncode == 0:
            metadata = json.loads(result.stdout)
            env_data = metadata.get(board, {})
            link_flags = env_data.get("link_flags", [])
            if link_flags:
                return {
                    "board": board,
                    "mcu": env_data.get("mcu", ""),
                    "platform": platform,
                    "source": f"PlatformIO {platform} platform (extracted via pio project metadata)",
                    "compiler_flags": {
                        "common": env_data.get("cc_flags", []),
                        "c": [],
                        "cxx": env_data.get("cxx_flags", []),
                    },
                    "defines": normalize_defines(env_data.get("defines", [])),
                    "linker_flags": link_flags,
                    "linker_libs": env_data.get("link_libs", []),
                }
    except (subprocess.TimeoutExpired, FileNotFoundError, json.JSONDecodeError):
        pass

    return None


def extract_build_flags(platform: str, board: str) -> dict | None:
    """Try platform_configs first (richer data), fall back to live PIO."""
    result = extract_from_platform_configs(platform, board)
    if result:
        return result

    result = extract_from_pio_envdump(platform, board)
    if result:
        return result

    print(f"  Warning: could not extract flags for {board} from any source", file=sys.stderr)
    return None


def load_mcu_config(platform: str, board: str) -> dict | None:
    """Load fbuild's Rust MCU config for the given board."""
    info = PLATFORMS.get(platform, {})
    board_map = info.get("board_to_mcu_config", {})
    family = board_map.get(board)
    if family is None:
        return None

    configs_dir = get_mcu_configs_dir(platform)
    if not configs_dir:
        return None

    config_path = configs_dir / f"{family}.json"
    if not config_path.exists():
        return None
    return json.loads(config_path.read_text(encoding="utf-8"))


def validate_compiler_flags(reference: dict, mcu_config: dict) -> list[str]:
    """Compare reference compiler flags against fbuild's MCU config (superset check)."""
    issues = []
    ref_cf = reference.get("compiler_flags", {})
    mcu_cf = mcu_config.get("compiler_flags", {})

    for category in ("common", "c", "cxx"):
        ref_flags = set(ref_cf.get(category, []))
        mcu_flags = set(mcu_cf.get(category, []))
        missing = ref_flags - mcu_flags
        for flag in sorted(missing):
            issues.append(f"missing compiler_flags.{category}: {flag}")

    return issues


def validate_linker_flags(reference: dict, mcu_config: dict) -> list[str]:
    """Compare reference linker flags against fbuild's MCU config."""
    issues = []

    ref_flags = set(reference.get("linker_flags", []))
    mcu_flags = set(mcu_config.get("linker_flags", []))
    for flag in sorted(ref_flags - mcu_flags):
        issues.append(f"missing linker flag: {flag}")

    ref_libs = set(reference.get("linker_libs", []))
    mcu_libs = set(mcu_config.get("linker_libs", []))
    for lib in sorted(ref_libs - mcu_libs):
        issues.append(f"missing linker lib: {lib}")

    return issues


def validate_all(reference: dict, mcu_config: dict) -> list[str]:
    """Run all validation checks."""
    issues = []
    issues.extend(validate_compiler_flags(reference, mcu_config))
    issues.extend(validate_linker_flags(reference, mcu_config))
    # Note: defines are validated in Rust tests (they come from BoardConfig::get_defines(),
    # not from MCU config JSON), so we skip define validation here.
    return issues


def write_reference(platform: str, board: str, data: dict) -> None:
    """Write a reference JSON file to the appropriate platform directory."""
    ref_dir = get_reference_dir(platform)
    ref_dir.mkdir(parents=True, exist_ok=True)
    out_path = ref_dir / f"{board}.json"
    out_path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    print(f"  Wrote {out_path.relative_to(REPO_ROOT)}")


def main() -> int:
    args = sys.argv[1:]
    boards: list[tuple[str, str]] = []  # (platform, board) pairs
    platforms_filter: list[str] = []
    validate_only = False

    i = 0
    while i < len(args):
        if args[i] == "--board" and i + 1 < len(args):
            board = args[i + 1]
            platform = get_platform_for_board(board)
            if not platform:
                print(f"Error: board '{board}' not found in any platform_configs", file=sys.stderr)
                return 1
            boards.append((platform, board))
            i += 2
        elif args[i] == "--platform" and i + 1 < len(args):
            platforms_filter.append(args[i + 1])
            i += 2
        elif args[i] == "--all":
            boards = find_all_boards()
            i += 1
        elif args[i] == "--validate":
            validate_only = True
            i += 1
        else:
            print(f"Unknown argument: {args[i]}", file=sys.stderr)
            print(__doc__, file=sys.stderr)
            return 1

    # Apply platform filter
    if platforms_filter and not boards:
        for pf in platforms_filter:
            boards.extend(find_boards_for_platform(pf))
    elif not boards:
        boards = find_all_boards()

    if platforms_filter and boards:
        boards = [(p, b) for p, b in boards if p in platforms_filter]

    if not boards:
        print("No boards found.", file=sys.stderr)
        return 1

    # Group by platform for display
    by_platform: dict[str, list[str]] = {}
    for platform, board in boards:
        by_platform.setdefault(platform, []).append(board)

    for platform in sorted(by_platform):
        board_list = by_platform[platform]
        print(f"[{platform}] {', '.join(board_list)}")
    print()

    total_issues = 0
    total_extracted = 0
    total_validated = 0
    total_skipped = 0

    for platform, board in boards:
        print(f"{platform}/{board}:")

        if validate_only:
            ref_dir = get_reference_dir(platform)
            ref_path = ref_dir / f"{board}.json"
            if not ref_path.exists():
                print(f"  No reference file at {ref_path.relative_to(REPO_ROOT)}")
                total_issues += 1
                continue
            reference = json.loads(ref_path.read_text(encoding="utf-8"))
        else:
            reference = extract_build_flags(platform, board)
            if not reference:
                total_issues += 1
                continue
            write_reference(platform, board, reference)
            total_extracted += 1

        mcu_config = load_mcu_config(platform, board)
        if not mcu_config:
            print(f"  Skipped validation (no fbuild MCU config)")
            total_skipped += 1
        else:
            issues = validate_all(reference, mcu_config)
            if issues:
                for issue in issues:
                    print(f"  FAIL: {issue}")
                total_issues += len(issues)
            else:
                print(f"  OK: all reference flags present in MCU config")
                total_validated += 1

        print()

    # Summary
    print("─" * 50)
    if not validate_only:
        print(f"Extracted: {total_extracted} reference configs")
    print(f"Validated: {total_validated} (all flags match)")
    if total_skipped:
        print(f"Skipped:   {total_skipped} (no fbuild MCU config yet)")
    if total_issues:
        print(f"FAILED:    {total_issues} issue(s) found")
        return 1

    print("All boards OK.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

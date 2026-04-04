#!/usr/bin/env python3
"""Extract linker flags from PlatformIO for Teensy boards and write reference JSONs.

This script runs `pio run -v --dry-run` (or falls back to parsing the
platform_configs in build/lib/) to extract the authoritative linker flags
that PlatformIO uses for each board. The output is written to
crates/fbuild-build/src/teensy/configs/reference/<board>.json.

Usage:
    # Extract from PlatformIO (requires `pio` installed and platforms downloaded):
    uv run python ci/extract_pio_linker_flags.py --board teensy36 --board teensy41

    # Extract all boards that have test projects:
    uv run python ci/extract_pio_linker_flags.py --all

    # Validate only (compare existing reference against fbuild configs, don't write):
    uv run python ci/extract_pio_linker_flags.py --validate
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
REFERENCE_DIR = REPO_ROOT / "crates" / "fbuild-build" / "src" / "teensy" / "configs" / "reference"
MCU_CONFIGS_DIR = REPO_ROOT / "crates" / "fbuild-build" / "src" / "teensy" / "configs"
TEST_PROJECTS_DIR = REPO_ROOT / "tests"
PLATFORM_CONFIGS_DIR = REPO_ROOT / "build" / "lib" / "fbuild" / "platform_configs" / "teensy"

# Map board IDs to the Rust MCU config family they should use
BOARD_TO_MCU_CONFIG = {
    "teensy36": "teensy3x",
    "teensy35": "teensy3x",
    "teensy31": "teensy3x",
    "teensy30": "teensy3x",
    "teensy41": "teensy4x",
    "teensy40": "teensy4x",
    "teensylc": "teensylc",
}


def find_teensy_test_boards() -> list[str]:
    """Find all Teensy test projects in the tests/ directory."""
    boards = []
    if TEST_PROJECTS_DIR.exists():
        for entry in sorted(TEST_PROJECTS_DIR.iterdir()):
            if entry.is_dir() and entry.name.startswith("teensy"):
                ini = entry / "platformio.ini"
                if ini.exists():
                    boards.append(entry.name)
    return boards


def extract_from_platform_configs(board: str) -> dict | None:
    """Extract linker flags from the Python platform_configs (build artifacts)."""
    config_path = PLATFORM_CONFIGS_DIR / f"{board}.json"
    if not config_path.exists():
        return None
    data = json.loads(config_path.read_text(encoding="utf-8"))
    return {
        "board": board,
        "mcu": data.get("mcu", ""),
        "source": "PlatformIO teensy platform (extracted from SCons builder environment)",
        "linker_flags": data.get("linker_flags", []),
        "linker_libs": data.get("linker_libs", []),
    }


def extract_from_pio_envdump(board: str) -> dict | None:
    """Extract linker flags by running PlatformIO's environment dump."""
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
            link_libs = env_data.get("link_libs", [])
            if link_flags:
                return {
                    "board": board,
                    "mcu": env_data.get("mcu", ""),
                    "source": "PlatformIO teensy platform (extracted via pio project metadata)",
                    "linker_flags": link_flags,
                    "linker_libs": link_libs,
                }
    except (subprocess.TimeoutExpired, FileNotFoundError, json.JSONDecodeError):
        pass

    return None


def extract_linker_flags(board: str) -> dict | None:
    """Try PlatformIO first, fall back to platform_configs."""
    result = extract_from_pio_envdump(board)
    if result:
        return result

    result = extract_from_platform_configs(board)
    if result:
        return result

    print(f"  Warning: could not extract flags for {board} from any source", file=sys.stderr)
    return None


def load_mcu_config(board: str) -> dict | None:
    """Load fbuild's Rust MCU config for the given board."""
    family = BOARD_TO_MCU_CONFIG.get(board)
    if not family:
        print(f"  Warning: no MCU config family mapping for board '{board}'", file=sys.stderr)
        return None
    config_path = MCU_CONFIGS_DIR / f"{family}.json"
    if not config_path.exists():
        return None
    return json.loads(config_path.read_text(encoding="utf-8"))


def validate_flags(reference: dict, mcu_config: dict) -> list[str]:
    """Compare reference linker flags against fbuild's MCU config. Returns list of issues."""
    issues = []
    ref_flags = set(reference.get("linker_flags", []))
    mcu_flags = set(mcu_config.get("linker_flags", []))

    missing = ref_flags - mcu_flags
    for flag in sorted(missing):
        issues.append(f"missing linker flag: {flag}")

    ref_libs = set(reference.get("linker_libs", []))
    mcu_libs = set(mcu_config.get("linker_libs", []))

    missing_libs = ref_libs - mcu_libs
    for lib in sorted(missing_libs):
        issues.append(f"missing linker lib: {lib}")

    return issues


def write_reference(board: str, data: dict) -> None:
    """Write a reference JSON file."""
    REFERENCE_DIR.mkdir(parents=True, exist_ok=True)
    out_path = REFERENCE_DIR / f"{board}.json"
    out_path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    print(f"  Wrote {out_path.relative_to(REPO_ROOT)}")


def main() -> int:
    args = sys.argv[1:]
    boards: list[str] = []
    validate_only = False

    i = 0
    while i < len(args):
        if args[i] == "--board" and i + 1 < len(args):
            boards.append(args[i + 1])
            i += 2
        elif args[i] == "--all":
            boards = find_teensy_test_boards()
            i += 1
        elif args[i] == "--validate":
            validate_only = True
            boards = find_teensy_test_boards()
            i += 1
        else:
            print(f"Unknown argument: {args[i]}", file=sys.stderr)
            print(__doc__, file=sys.stderr)
            return 1

    if not boards:
        boards = find_teensy_test_boards()
        if not boards:
            print("No Teensy test projects found in tests/", file=sys.stderr)
            return 1

    print(f"Boards: {', '.join(boards)}")
    print()

    total_issues = 0

    for board in boards:
        print(f"{board}:")

        if validate_only:
            # Load existing reference
            ref_path = REFERENCE_DIR / f"{board}.json"
            if not ref_path.exists():
                print(f"  No reference file at {ref_path.relative_to(REPO_ROOT)}")
                total_issues += 1
                continue
            reference = json.loads(ref_path.read_text(encoding="utf-8"))
        else:
            reference = extract_linker_flags(board)
            if not reference:
                total_issues += 1
                continue
            write_reference(board, reference)

        mcu_config = load_mcu_config(board)
        if not mcu_config:
            print(f"  Warning: no MCU config found for {board}")
            total_issues += 1
            continue

        issues = validate_flags(reference, mcu_config)
        if issues:
            for issue in issues:
                print(f"  FAIL: {issue}")
            total_issues += len(issues)
        else:
            print(f"  OK: all reference flags present in MCU config")

        print()

    if total_issues:
        print(f"FAILED: {total_issues} issue(s) found.")
        print(f"Fix the MCU configs in {MCU_CONFIGS_DIR.relative_to(REPO_ROOT)}/ to match PlatformIO.")
        return 1

    print("All boards validated successfully.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

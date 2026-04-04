#!/usr/bin/env python3
"""Fetch board lists from external sources and compare against fbuild's database.

Downloads Arduino package index JSON files and Zephyr board listings, extracts
board names, and reports boards that exist externally but are missing from
fbuild's board database.

Usage:
    python ci/board_sources.py --list-arduino       # List Arduino boards
    python ci/board_sources.py --list-zephyr        # List Zephyr boards
    python ci/board_sources.py --search QUERY       # Search all sources
    python ci/board_sources.py --compare            # Find missing boards
    python ci/board_sources.py --compare --json     # Output as JSON

Requires internet access. Uses only stdlib (urllib), no extra dependencies.
"""

from __future__ import annotations

import json
import re
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

# ---------------------------------------------------------------------------
# Arduino package index URLs
# ---------------------------------------------------------------------------

ARDUINO_PACKAGE_INDICES: dict[str, str] = {
    "arduino_official": "https://downloads.arduino.cc/packages/package_index.json",
    "esp32_espressif": "https://raw.githubusercontent.com/espressif/arduino-esp32/gh-pages/package_esp32_index.json",
    "esp8266": "https://arduino.esp8266.com/stable/package_esp8266com_index.json",
    "adafruit": "https://adafruit.github.io/arduino-board-index/package_adafruit_index.json",
    "sparkfun": "https://raw.githubusercontent.com/sparkfun/Arduino_Boards/main/IDE_Board_Manager/package_sparkfun_index.json",
    "teensy": "https://www.pjrc.com/teensy/package_teensy_index.json",
}

# Zephyr boards are listed via the GitHub API (directory listing)
ZEPHYR_BOARDS_API = "https://api.github.com/repos/zephyrproject-rtos/zephyr/contents/boards"

# ---------------------------------------------------------------------------
# Data types
# ---------------------------------------------------------------------------


@dataclass
class ExternalBoard:
    """A board found in an external source."""

    name: str
    source: str  # e.g. "arduino:esp32_espressif", "zephyr:espressif"
    architecture: str = ""
    vendor: str = ""


@dataclass
class SourceReport:
    """Report from fetching a single source."""

    source_id: str
    boards: list[ExternalBoard] = field(default_factory=list)
    error: str | None = None


# ---------------------------------------------------------------------------
# Fetching helpers
# ---------------------------------------------------------------------------


def _fetch_json(url: str, timeout: int = 30) -> object:
    """Fetch a URL and parse as JSON."""
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-board-sources/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _fetch_json_safe(url: str, timeout: int = 30) -> tuple[object | None, str | None]:
    """Fetch JSON, returning (data, None) or (None, error_message)."""
    try:
        return _fetch_json(url, timeout), None
    except (urllib.error.URLError, json.JSONDecodeError, OSError) as exc:
        return None, str(exc)


# ---------------------------------------------------------------------------
# Arduino package index parsing
# ---------------------------------------------------------------------------


def fetch_arduino_boards(source_id: str, url: str) -> SourceReport:
    """Parse an Arduino package_index.json and extract board names.

    Deduplicates boards — the same board name appears across multiple platform
    versions in the package index.
    """
    report = SourceReport(source_id=f"arduino:{source_id}")
    data, err = _fetch_json_safe(url, timeout=60)
    if err:
        report.error = f"Failed to fetch {url}: {err}"
        return report

    if not isinstance(data, dict):
        report.error = f"Unexpected JSON type from {url}"
        return report

    # Deduplicate by (name, architecture) — same board listed in every version
    seen: set[tuple[str, str]] = set()

    packages = data.get("packages", [])
    for pkg in packages:
        if not isinstance(pkg, dict):
            continue
        packager = pkg.get("name", "unknown")
        platforms = pkg.get("platforms", [])
        for platform in platforms:
            if not isinstance(platform, dict):
                continue
            arch = platform.get("architecture", "")
            boards = platform.get("boards", [])
            for board in boards:
                if isinstance(board, dict) and "name" in board:
                    key = (board["name"], arch)
                    if key in seen:
                        continue
                    seen.add(key)
                    report.boards.append(
                        ExternalBoard(
                            name=board["name"],
                            source=report.source_id,
                            architecture=arch,
                            vendor=packager,
                        )
                    )

    return report


def fetch_all_arduino() -> list[SourceReport]:
    """Fetch boards from all Arduino package indices."""
    reports: list[SourceReport] = []
    for source_id, url in ARDUINO_PACKAGE_INDICES.items():
        print(f"  Fetching arduino:{source_id}...", file=sys.stderr, flush=True)
        reports.append(fetch_arduino_boards(source_id, url))
    return reports


# ---------------------------------------------------------------------------
# Zephyr board parsing
# ---------------------------------------------------------------------------


def fetch_zephyr_boards() -> SourceReport:
    """Fetch board names from Zephyr's GitHub repo (boards/ directory listing)."""
    report = SourceReport(source_id="zephyr")
    print("  Fetching zephyr boards...", file=sys.stderr, flush=True)

    # Get top-level vendor directories under boards/
    data, err = _fetch_json_safe(ZEPHYR_BOARDS_API, timeout=30)
    if err:
        report.error = f"Failed to fetch Zephyr boards index: {err}"
        return report

    if not isinstance(data, list):
        report.error = "Unexpected response from Zephyr boards API"
        return report

    for entry in data:
        if not isinstance(entry, dict):
            continue
        if entry.get("type") != "dir":
            continue
        vendor = entry.get("name", "")
        # Skip non-vendor directories
        if vendor in ("common", "shields", "snippets"):
            continue

        # Fetch board directories under each vendor
        vendor_url = entry.get("url", "")
        if not vendor_url:
            continue

        vendor_data, vendor_err = _fetch_json_safe(vendor_url, timeout=30)
        if vendor_err or not isinstance(vendor_data, list):
            continue

        for board_entry in vendor_data:
            if not isinstance(board_entry, dict):
                continue
            if board_entry.get("type") != "dir":
                continue
            board_name = board_entry.get("name", "")
            if board_name:
                report.boards.append(
                    ExternalBoard(
                        name=board_name,
                        source="zephyr",
                        vendor=vendor,
                    )
                )

    return report


# ---------------------------------------------------------------------------
# fbuild database
# ---------------------------------------------------------------------------


def load_fbuild_boards() -> set[str]:
    """Load the set of board IDs from fbuild's manifest."""
    manifest_path = (
        Path(__file__).resolve().parent.parent
        / "crates"
        / "fbuild-config"
        / "assets"
        / "boards"
        / "manifest.json"
    )
    if not manifest_path.exists():
        print(f"Warning: manifest not found at {manifest_path}", file=sys.stderr)
        return set()
    with manifest_path.open() as f:
        ids = json.load(f)
    return set(ids) if isinstance(ids, list) else set()


def load_fbuild_board_names() -> dict[str, str]:
    """Load a mapping of board_id -> display name from fbuild's JSON files."""
    boards_dir = (
        Path(__file__).resolve().parent.parent
        / "crates"
        / "fbuild-config"
        / "assets"
        / "boards"
        / "json"
    )
    mapping: dict[str, str] = {}
    if not boards_dir.exists():
        return mapping
    for p in boards_dir.glob("*.json"):
        if p.name == "manifest.json":
            continue
        try:
            data = json.loads(p.read_text(encoding="utf-8"))
            board_id = data.get("id", p.stem)
            name = data.get("name", board_id)
            mapping[board_id] = name
        except (json.JSONDecodeError, OSError):
            continue
    return mapping


# ---------------------------------------------------------------------------
# Comparison logic
# ---------------------------------------------------------------------------


def normalize_for_matching(name: str) -> str:
    """Normalize a board name for fuzzy matching."""
    s = name.lower()
    s = re.sub(r"[^a-z0-9]", "", s)
    return s


def compare_boards(
    external_reports: list[SourceReport], fbuild_ids: set[str], fbuild_names: dict[str, str]
) -> dict[str, list[ExternalBoard]]:
    """Find external boards with no match in fbuild.

    Returns a dict of source_id -> list of unmatched ExternalBoard.
    """
    # Build normalized lookup sets
    normalized_ids = {normalize_for_matching(bid) for bid in fbuild_ids}
    normalized_names = {normalize_for_matching(name) for name in fbuild_names.values()}
    all_normalized = normalized_ids | normalized_names

    missing: dict[str, list[ExternalBoard]] = {}
    for report in external_reports:
        if report.error:
            continue
        for board in report.boards:
            norm = normalize_for_matching(board.name)
            if norm and norm not in all_normalized:
                missing.setdefault(report.source_id, []).append(board)

    return missing


def search_boards(
    query: str, external_reports: list[SourceReport], fbuild_names: dict[str, str]
) -> list[dict]:
    """Search for a board across all sources (external + fbuild)."""
    q = query.lower()
    results: list[dict] = []

    # Search fbuild
    for board_id, name in fbuild_names.items():
        if q in board_id.lower() or q in name.lower():
            results.append({"source": "fbuild", "id": board_id, "name": name})

    # Search external
    for report in external_reports:
        if report.error:
            continue
        for board in report.boards:
            if q in board.name.lower() or q in board.vendor.lower():
                results.append(
                    {
                        "source": report.source_id,
                        "name": board.name,
                        "architecture": board.architecture,
                        "vendor": board.vendor,
                    }
                )

    return results


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def print_source_report(report: SourceReport) -> None:
    """Pretty-print a single source report."""
    if report.error:
        print(f"\n{report.source_id}: ERROR - {report.error}")
        return
    print(f"\n{report.source_id}: {len(report.boards)} boards")
    for board in sorted(report.boards, key=lambda b: b.name):
        parts = [f"  {board.name}"]
        if board.architecture:
            parts.append(f"[{board.architecture}]")
        if board.vendor:
            parts.append(f"({board.vendor})")
        print(" ".join(parts))


def main() -> int:
    args = sys.argv[1:]

    if not args or "--help" in args or "-h" in args:
        print(__doc__)
        return 0

    output_json = "--json" in args
    args = [a for a in args if a != "--json"]

    if "--list-arduino" in args:
        reports = fetch_all_arduino()
        if output_json:
            out = []
            for r in reports:
                out.append(
                    {
                        "source": r.source_id,
                        "error": r.error,
                        "boards": [
                            {"name": b.name, "architecture": b.architecture, "vendor": b.vendor}
                            for b in r.boards
                        ],
                    }
                )
            print(json.dumps(out, indent=2))
        else:
            for r in reports:
                print_source_report(r)
        return 0

    if "--list-zephyr" in args:
        report = fetch_zephyr_boards()
        if output_json:
            print(
                json.dumps(
                    {
                        "source": report.source_id,
                        "error": report.error,
                        "boards": [
                            {"name": b.name, "vendor": b.vendor} for b in report.boards
                        ],
                    },
                    indent=2,
                )
            )
        else:
            print_source_report(report)
        return 0

    if "--search" in args:
        idx = args.index("--search")
        if idx + 1 >= len(args):
            print("Error: --search requires a query argument", file=sys.stderr)
            return 1
        query = args[idx + 1]

        print(f"Searching for '{query}' across all sources...", file=sys.stderr)
        arduino_reports = fetch_all_arduino()
        zephyr_report = fetch_zephyr_boards()
        fbuild_names = load_fbuild_board_names()

        results = search_boards(query, arduino_reports + [zephyr_report], fbuild_names)
        if output_json:
            print(json.dumps(results, indent=2))
        else:
            if not results:
                print(f"No boards matching '{query}' found.")
            else:
                print(f"\nFound {len(results)} result(s) for '{query}':\n")
                for r in results:
                    source = r.get("source", "?")
                    name = r.get("name", "?")
                    extra = []
                    if r.get("id"):
                        extra.append(f"id={r['id']}")
                    if r.get("architecture"):
                        extra.append(f"arch={r['architecture']}")
                    if r.get("vendor"):
                        extra.append(f"vendor={r['vendor']}")
                    suffix = f" ({', '.join(extra)})" if extra else ""
                    print(f"  [{source}] {name}{suffix}")
        return 0

    if "--compare" in args:
        print("Fetching external board lists...", file=sys.stderr)
        arduino_reports = fetch_all_arduino()
        zephyr_report = fetch_zephyr_boards()
        all_reports = arduino_reports + [zephyr_report]

        fbuild_ids = load_fbuild_boards()
        fbuild_names = load_fbuild_board_names()

        missing = compare_boards(all_reports, fbuild_ids, fbuild_names)

        # Summary
        total_external = sum(
            len(r.boards) for r in all_reports if not r.error
        )
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
            print("Board Coverage Comparison")
            print(f"{'=' * 60}")
            print(f"  fbuild boards:   {len(fbuild_ids)}")
            print(f"  External boards: {total_external}")
            print(f"  Missing from fbuild: {total_missing}")

            if errors:
                print(f"\n  Errors ({len(errors)}):")
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

    print(f"Unknown arguments: {' '.join(args)}", file=sys.stderr)
    print(__doc__, file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())

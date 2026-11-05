#!/usr/bin/env python3
"""Assert each ESP32 chip config's flash offsets match an authoritative source.

The second-stage **bootloader offset** (`esptool.flash_offsets.bootloader` in
`crates/fbuild-build-esp/src/esp32/configs/esp32*.json`) is a ROM-defined
constant: get it wrong and the chip's ROM reads garbage at its fixed load
address and enters an `invalid header: 0x...` reboot loop (see #278: esp32p4 /
esp32c5 shipped `0x0` instead of `0x2000` and bricked boot).

These values are hand-maintained, so nothing previously cross-checked them
against an external authority. This script does:

  - Reads every `esp32*.json` config and its `esptool.flash_offsets`.
  - Determines the AUTHORITATIVE `build.bootloader_addr` per chip from the
    arduino-esp32 `boards.txt` (with the `platform.txt`
    `build.bootloader_addr` default applied when a chip has no explicit
    override).
  - HARD-FAILS (non-zero exit) on any mismatch, on any config chip that has no
    corresponding `boards.txt` entry, and if it cannot locate an authoritative
    source at all (it never silently passes).
  - HARD-FAILS when a chip relies on the `platform.txt` default but no
    `platform.txt` declaring `build.bootloader_addr` is available. There is no
    hard-coded fallback: an unavailable default is an unverifiable chip, and
    substituting a constant that came from nowhere would let a wrong offset
    pass silently.
  - Also asserts `partitions == 0x8000` and `firmware == 0x10000` (fixed in
    arduino-esp32 `platform.txt` flash recipes).

Authoritative source (in priority order):
  1. --boards-txt <path>                (explicit boards.txt)
  2. $FBUILD_ESP32_BOARDS_TXT           (explicit boards.txt via env)
  3. Installed framework in the fbuild cache
     (~/.fbuild/{dev,prod}/cache/platforms/framework-arduinoespressif32/.../
      esp32-core-*/boards.txt), highest version wins.
  4. --download [version]               (fetch boards.txt + platform.txt from
                                         the pioarduino/arduino-esp32 release
                                         tag; requires internet). This is what
                                         CI uses, since the framework is not
                                         pre-installed in the validate job.

Reference for maintainers: the offset is `ESP_BOOTLOADER_OFFSET` in ESP-IDF and
is exposed as `<chip>.build.bootloader_addr` in arduino-esp32 `boards.txt`.

Usage:
    python ci/check_flash_offsets.py                       # auto-discover source
    python ci/check_flash_offsets.py --boards-txt PATH     # explicit boards.txt
    python ci/check_flash_offsets.py --download            # fetch from pioarduino
    python ci/check_flash_offsets.py --download 3.3.7      # fetch a specific tag
"""

from __future__ import annotations

import json
import os
import re
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

# Default pioarduino arduino-esp32 release tag used when --download has no
# explicit version. Bump alongside the framework version fbuild ships.
DEFAULT_DOWNLOAD_VERSION = "3.3.7"

# Fixed offsets from arduino-esp32 platform.txt flash recipes (not per-chip).
EXPECTED_PARTITIONS = "0x8000"
EXPECTED_FIRMWARE = "0x10000"

PIOARDUINO_RAW = "https://raw.githubusercontent.com/pioarduino/arduino-esp32/refs/tags/{version}/{name}"


class UsageError(Exception):
    """Bad command line."""


class SourceError(Exception):
    """No authoritative boards.txt could be resolved."""


@dataclass
class Options:
    """Parsed command line."""

    boards_txt: str | None = None
    download_version: str | None = None
    help_requested: bool = False


@dataclass
class Source:
    """An authoritative boards.txt plus the platform.txt that may accompany it.

    `platform_text is None` means no platform.txt is available at all; that is
    distinct from a platform.txt that exists but does not declare
    `build.bootloader_addr`. Both leave the per-chip default unresolved.
    """

    boards_text: str
    platform_text: str | None
    description: str


@dataclass
class ConfigResult:
    """Per-config outcome: the printable row and any failures."""

    row: str
    failures: list[str] = field(default_factory=list)


def home_dir() -> Path:
    home = (
        os.environ.get("USERPROFILE")
        if sys.platform == "win32"
        else os.environ.get("HOME")
    )
    return Path(home or "")


def configs_dir() -> Path:
    """Locate the fbuild esp32 config directory relative to this script."""
    return (
        Path(__file__).resolve().parent.parent
        / "crates"
        / "fbuild-build-esp"
        / "src"
        / "esp32"
        / "configs"
    )


def normalize_offset(value: str) -> str:
    """Normalize a hex offset string for comparison ('0x1000' == '0x1000')."""
    s = value.strip().lower()
    if s.startswith("0x"):
        # Drop leading zeros after 0x but keep at least one digit.
        digits = s[2:].lstrip("0") or "0"
        return "0x" + digits
    return s


# ---------------------------------------------------------------------------
# Authoritative source discovery
# ---------------------------------------------------------------------------


def _version_key(version: str) -> tuple[int, ...]:
    parts = re.findall(r"\d+", version)
    return tuple(int(p) for p in parts) if parts else (0,)


def find_cached_framework() -> tuple[Path, Path] | None:
    """Find the highest-version installed arduino-esp32 framework in the cache.

    Returns (boards_txt, platform_txt) or None if not found. Mirrors fbuild's
    path layout: ~/.fbuild/{dev,prod}/cache/platforms/
        framework-arduinoespressif32/<hash>/<version>/esp32-core-<version>/.
    """
    home = home_dir()
    if not home:
        return None

    candidates: list[tuple[tuple[int, ...], Path]] = []
    search_roots = [
        home
        / ".fbuild"
        / "dev"
        / "cache"
        / "platforms"
        / "framework-arduinoespressif32",
        home
        / ".fbuild"
        / "prod"
        / "cache"
        / "platforms"
        / "framework-arduinoespressif32",
    ]
    for root in search_roots:
        if not root.exists():
            continue
        for boards_txt in root.glob("**/boards.txt"):
            platform_txt = boards_txt.with_name("platform.txt")
            if not platform_txt.exists():
                continue
            # Derive a version key from the path (esp32-core-X.Y.Z).
            m = re.search(r"esp32-core-([\d.]+)", str(boards_txt))
            version = m.group(1) if m else "0"
            candidates.append((_version_key(version), boards_txt))

    if not candidates:
        return None

    candidates.sort(key=lambda c: c[0])
    best = candidates[-1][1]
    return best, best.with_name("platform.txt")


def download_authoritative_text(version: str) -> tuple[str, str]:
    """Fetch boards.txt and platform.txt from the pioarduino release tag."""
    out: list[str] = []
    for name in ("boards.txt", "platform.txt"):
        url = PIOARDUINO_RAW.format(version=version, name=name)
        req = urllib.request.Request(
            url, headers={"User-Agent": "fbuild-check-flash-offsets/1.0"}
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                out.append(resp.read().decode("utf-8"))
        except (urllib.error.URLError, OSError) as exc:
            raise RuntimeError(f"Failed to download {url}: {exc}") from exc
    return out[0], out[1]


def _read_optional(path: Path) -> str | None:
    return path.read_text(encoding="utf-8") if path.exists() else None


def parse_args(args: list[str]) -> Options:
    options = Options()
    i = 0
    while i < len(args):
        arg = args[i]
        if arg == "--boards-txt":
            if i + 1 >= len(args):
                raise UsageError("--boards-txt needs a path")
            options.boards_txt = args[i + 1]
            i += 2
        elif arg == "--download":
            # Optional version follows.
            options.download_version = DEFAULT_DOWNLOAD_VERSION
            if i + 1 < len(args) and not args[i + 1].startswith("-"):
                options.download_version = args[i + 1]
                i += 2
            else:
                i += 1
        elif arg in ("-h", "--help"):
            options.help_requested = True
            return options
        else:
            raise UsageError(f"Unknown argument: {arg}")
    return options


def resolve_source(options: Options) -> Source:
    """Resolve the authoritative boards.txt/platform.txt pair, or raise."""
    if options.download_version is not None:
        try:
            boards_text, platform_text = download_authoritative_text(
                options.download_version
            )
        except RuntimeError as exc:
            raise SourceError(str(exc)) from exc
        return Source(
            boards_text,
            platform_text,
            f"pioarduino/arduino-esp32 release tag {options.download_version} (downloaded)",
        )

    explicit = options.boards_txt or os.environ.get("FBUILD_ESP32_BOARDS_TXT")
    if explicit:
        boards_path = Path(explicit)
        if not boards_path.exists():
            raise SourceError(f"boards.txt not found at {boards_path}")
        return Source(
            boards_path.read_text(encoding="utf-8"),
            _read_optional(boards_path.with_name("platform.txt")),
            f"explicit boards.txt: {boards_path}",
        )

    found = find_cached_framework()
    if found is not None:
        boards_path, platform_path = found
        return Source(
            boards_path.read_text(encoding="utf-8"),
            platform_path.read_text(encoding="utf-8"),
            f"installed framework: {boards_path}",
        )

    raise SourceError(
        "could not locate an authoritative arduino-esp32 boards.txt.\n"
        "  Tried: --boards-txt, $FBUILD_ESP32_BOARDS_TXT, and the fbuild cache\n"
        "  (~/.fbuild/{dev,prod}/cache/platforms/framework-arduinoespressif32/).\n"
        "  Install the ESP32 framework, pass --boards-txt PATH, or use --download."
    )


# ---------------------------------------------------------------------------
# Parsing
# ---------------------------------------------------------------------------


def parse_platform_default(platform_text: str | None) -> str | None:
    """Read `build.bootloader_addr` from platform.txt, or None when unresolved.

    There is deliberately NO fallback constant: `None` means the authoritative
    default could not be established, and every chip that depends on it becomes
    a hard failure instead of being compared against a guessed value.
    """
    for line in (platform_text or "").splitlines():
        line = line.strip()
        if line.startswith("build.bootloader_addr="):
            value = line.split("=", 1)[1].strip()
            if value:
                return value
    return None


def parse_boards_bootloader_addr(boards_text: str) -> dict[str, str]:
    """Map chip-id -> explicit build.bootloader_addr from boards.txt.

    Only top-level chip entries (e.g. `esp32c3.build.bootloader_addr=0x0`) are
    captured; menu/option-scoped keys (containing `.menu.`) are ignored.
    """
    result: dict[str, str] = {}
    pattern = re.compile(r"^([A-Za-z0-9_\-]+)\.build\.bootloader_addr=(.+)$")
    for raw in boards_text.splitlines():
        line = raw.strip()
        if ".menu." in line:
            continue
        m = pattern.match(line)
        if m:
            result[m.group(1)] = m.group(2).strip()
    return result


def parse_known_chips(boards_text: str) -> set[str]:
    """Set of chip families arduino-esp32 supports (distinct `build.mcu` values).

    Used to distinguish a chip that relies on the platform.txt default
    bootloader offset (known to boards.txt, no explicit override) from a chip
    boards.txt has no knowledge of at all (a real coverage gap -> failure).
    Menu/option-scoped keys (containing `.menu.`) are ignored.
    """
    chips: set[str] = set()
    pattern = re.compile(r"^[A-Za-z0-9_\-]+\.build\.mcu=(.+)$")
    for raw in boards_text.splitlines():
        line = raw.strip()
        if ".menu." in line:
            continue
        m = pattern.match(line)
        if m:
            chips.add(m.group(1).strip())
    return chips


def authoritative_offset(
    chip: str,
    boards_addrs: dict[str, str],
    known_chips: set[str],
    platform_default: str | None,
) -> str | None:
    """Resolve the authoritative bootloader offset for a chip.

    Precedence mirrors how arduino-esp32 itself resolves the value:
      1. explicit `<chip>.build.bootloader_addr` in boards.txt, else
      2. the platform.txt default (`build.bootloader_addr`) for a chip that
         boards.txt otherwise knows about (e.g. esp32/esp32s2, which don't
         override the default), else
      3. `None` -- either boards.txt has no knowledge of this chip, or the
         platform.txt default is unavailable. The offset cannot be verified, so
         the caller treats both as a hard failure.
    """
    if chip in boards_addrs:
        return boards_addrs[chip]
    if chip in known_chips and platform_default is not None:
        return platform_default
    return None


def platform_default_reason(source: Source) -> str:
    """Why `parse_platform_default` could not establish a value."""
    if source.platform_text is None:
        return "no platform.txt accompanies this boards.txt"
    return "platform.txt does not declare build.bootloader_addr"


def load_config_offsets(path: Path) -> tuple[str, dict[str, str]]:
    """Return (mcu, flash_offsets) for an esp32 config JSON."""
    data = json.loads(path.read_text(encoding="utf-8"))
    mcu = data.get("mcu", path.stem)
    offsets = data.get("esptool", {}).get("flash_offsets", {})
    return mcu, offsets


# ---------------------------------------------------------------------------
# Main check
# ---------------------------------------------------------------------------


def unresolved_failure(
    mcu: str,
    boards_addrs: dict[str, str],
    known_chips: set[str],
    source: Source,
    platform_default: str | None,
) -> str:
    if mcu not in known_chips and mcu not in boards_addrs:
        return (
            f"{mcu}: chip is unknown to the authoritative boards.txt (no "
            f"`{mcu}.build.mcu` or `{mcu}.build.bootloader_addr` entry) -- "
            f"cannot verify this chip's bootloader offset"
        )
    assert platform_default is None
    return (
        f"{mcu}: no authoritative bootloader offset -- boards.txt has no "
        f"`{mcu}.build.bootloader_addr` and the platform.txt default is "
        f"unavailable ({platform_default_reason(source)}); refusing to guess"
    )


def check_config(
    mcu: str,
    offsets: dict[str, str],
    source: Source,
    boards_addrs: dict[str, str],
    known_chips: set[str],
    platform_default: str | None,
) -> ConfigResult:
    cfg_boot = offsets.get("bootloader")
    cfg_part = offsets.get("partitions")
    cfg_fw = offsets.get("firmware")
    failures: list[str] = []

    auth = authoritative_offset(mcu, boards_addrs, known_chips, platform_default)
    if auth is None:
        failures.append(
            unresolved_failure(mcu, boards_addrs, known_chips, source, platform_default)
        )
    elif cfg_boot is None:
        failures.append(f"{mcu}: config has no esptool.flash_offsets.bootloader")
    elif normalize_offset(cfg_boot) != normalize_offset(auth):
        failures.append(
            f"{mcu}: bootloader offset {cfg_boot!r} != authoritative {auth!r} "
            f"(boards.txt build.bootloader_addr)"
        )

    if cfg_part is None or normalize_offset(cfg_part) != normalize_offset(
        EXPECTED_PARTITIONS
    ):
        failures.append(
            f"{mcu}: partitions offset {cfg_part!r} != expected {EXPECTED_PARTITIONS!r}"
        )
    if cfg_fw is None or normalize_offset(cfg_fw) != normalize_offset(
        EXPECTED_FIRMWARE
    ):
        failures.append(
            f"{mcu}: firmware offset {cfg_fw!r} != expected {EXPECTED_FIRMWARE!r}"
        )

    row = (
        f"{mcu:<12} {str(cfg_boot):<10} {auth if auth is not None else 'MISSING':<14} "
        f"{str(cfg_part):<8} {str(cfg_fw):<10} {'PASS' if not failures else 'FAIL'}"
    )
    return ConfigResult(row=row, failures=failures)


@dataclass
class Report:
    rows: list[str] = field(default_factory=list)
    failures: list[str] = field(default_factory=list)
    configs: int = 0


def evaluate(cfg_dir: Path, source: Source) -> Report:
    """Compare every esp32 config against `source`; no printing, no exit."""
    platform_default = parse_platform_default(source.platform_text)
    boards_addrs = parse_boards_bootloader_addr(source.boards_text)
    known_chips = parse_known_chips(source.boards_text)
    report = Report()
    for path in sorted(cfg_dir.glob("esp32*.json")):
        mcu, offsets = load_config_offsets(path)
        result = check_config(
            mcu, offsets, source, boards_addrs, known_chips, platform_default
        )
        report.rows.append(result.row)
        report.failures.extend(result.failures)
        report.configs += 1
    return report


def report_lines(report: Report, source: Source, cfg_dir: Path) -> str:
    platform_default = parse_platform_default(source.platform_text)
    resolved = platform_default if platform_default is not None else "UNAVAILABLE"
    header = f"{'CHIP':<12} {'CONFIG':<10} {'AUTHORITATIVE':<14} {'PART':<8} {'FW':<10} RESULT"
    body = [header, "-" * len(header), *report.rows]
    return "\n".join(
        [
            "Authoritative source: " + source.description,
            f"platform.txt default build.bootloader_addr = {resolved}",
            f"configs directory: {cfg_dir}",
            "",
            *body,
        ]
    )


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    try:
        options = parse_args(args)
    except UsageError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        print(__doc__, file=sys.stderr)
        return 1

    if options.help_requested:
        print(__doc__)
        return 0

    cfg_dir = configs_dir()
    if not cfg_dir.exists():
        print(f"Error: esp32 configs not found at {cfg_dir}", file=sys.stderr)
        return 1

    try:
        source = resolve_source(options)
    except SourceError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 1

    report = evaluate(cfg_dir, source)
    if report.configs == 0:
        print(f"Error: no esp32*.json configs found in {cfg_dir}", file=sys.stderr)
        return 1

    print(report_lines(report, source, cfg_dir))
    print()
    if report.failures:
        print(f"FAILED: {len(report.failures)} flash-offset problem(s):")
        for failure in report.failures:
            print(f"  - {failure}")
        print()
        print(
            "The bootloader offset is a ROM-defined constant. The authoritative value is\n"
            "`<chip>.build.bootloader_addr` in arduino-esp32 boards.txt (ESP-IDF "
            "ESP_BOOTLOADER_OFFSET).\n"
            "Fix the config to match the authoritative source; do NOT guess."
        )
        return 1

    print(
        f"All {report.configs} esp32 config(s) match the authoritative flash offsets."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

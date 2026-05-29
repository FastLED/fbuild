#!/usr/bin/env python3
"""Assert each ESP32 chip config's flash offsets match an authoritative source.

The second-stage **bootloader offset** (`esptool.flash_offsets.bootloader` in
`crates/fbuild-build/src/esp32/configs/esp32*.json`) is a ROM-defined constant:
get it wrong and the chip's ROM reads garbage at its fixed load address and
enters an `invalid header: 0x...` reboot loop (see #278: esp32p4/esp32c5
shipped `0x0` instead of `0x2000` and bricked boot).

These values are hand-maintained, so nothing previously cross-checked them
against an external authority. This script does:

  - Reads every `esp32*.json` config and its `esptool.flash_offsets`.
  - Determines the AUTHORITATIVE `build.bootloader_addr` per chip from the
    arduino-esp32 `boards.txt` (with the `platform.txt`
    `build.bootloader_addr=0x1000` default applied when a chip has no explicit
    override).
  - HARD-FAILS (non-zero exit) on any mismatch, on any config chip that has no
    corresponding `boards.txt` entry, and if it cannot locate an authoritative
    source at all (it never silently passes).
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
from pathlib import Path

# Default pioarduino arduino-esp32 release tag used when --download has no
# explicit version. Bump alongside the framework version fbuild ships.
DEFAULT_DOWNLOAD_VERSION = "3.3.7"

# arduino-esp32 platform.txt default for chips that don't override the offset.
DEFAULT_BOOTLOADER_ADDR = "0x1000"

# Fixed offsets from arduino-esp32 platform.txt flash recipes (not per-chip).
EXPECTED_PARTITIONS = "0x8000"
EXPECTED_FIRMWARE = "0x10000"

PIOARDUINO_RAW = "https://raw.githubusercontent.com/pioarduino/arduino-esp32/refs/tags/{version}/{name}"


def home_dir() -> Path:
    home = os.environ.get("USERPROFILE") if sys.platform == "win32" else os.environ.get("HOME")
    return Path(home or "")


def configs_dir() -> Path:
    """Locate the fbuild esp32 config directory relative to this script."""
    return (
        Path(__file__).resolve().parent.parent
        / "crates"
        / "fbuild-build"
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
        home / ".fbuild" / "dev" / "cache" / "platforms" / "framework-arduinoespressif32",
        home / ".fbuild" / "prod" / "cache" / "platforms" / "framework-arduinoespressif32",
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
        req = urllib.request.Request(url, headers={"User-Agent": "fbuild-check-flash-offsets/1.0"})
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                out.append(resp.read().decode("utf-8"))
        except (urllib.error.URLError, OSError) as exc:
            raise RuntimeError(f"Failed to download {url}: {exc}") from exc
    return out[0], out[1]


# ---------------------------------------------------------------------------
# Parsing
# ---------------------------------------------------------------------------


def parse_platform_default(platform_text: str) -> str:
    """Read `build.bootloader_addr` default from platform.txt, if present."""
    for line in platform_text.splitlines():
        line = line.strip()
        if line.startswith("build.bootloader_addr="):
            return line.split("=", 1)[1].strip()
    return DEFAULT_BOOTLOADER_ADDR


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
    platform_default: str,
) -> str | None:
    """Resolve the authoritative bootloader offset for a chip.

    Precedence mirrors how arduino-esp32 itself resolves the value:
      1. explicit `<chip>.build.bootloader_addr` in boards.txt, else
      2. the platform.txt default (`build.bootloader_addr`) for a chip that
         boards.txt otherwise knows about (e.g. esp32/esp32s2, which don't
         override the default), else
      3. `None` -- boards.txt has no knowledge of this chip, so the offset
         cannot be verified; the caller treats this as a hard failure.
    """
    if chip in boards_addrs:
        return boards_addrs[chip]
    if chip in known_chips:
        return platform_default
    return None


# ---------------------------------------------------------------------------
# Main check
# ---------------------------------------------------------------------------


def load_config_offsets(path: Path) -> tuple[str, dict[str, str]]:
    """Return (mcu, flash_offsets) for an esp32 config JSON."""
    data = json.loads(path.read_text(encoding="utf-8"))
    mcu = data.get("mcu", path.stem)
    offsets = data.get("esptool", {}).get("flash_offsets", {})
    return mcu, offsets


def main() -> int:
    args = sys.argv[1:]
    boards_txt_arg: str | None = None
    download_version: str | None = None
    i = 0
    while i < len(args):
        if args[i] == "--boards-txt" and i + 1 < len(args):
            boards_txt_arg = args[i + 1]
            i += 2
        elif args[i] == "--download":
            # Optional version follows.
            if i + 1 < len(args) and not args[i + 1].startswith("-"):
                download_version = args[i + 1]
                i += 2
            else:
                download_version = DEFAULT_DOWNLOAD_VERSION
                i += 1
        elif args[i] in ("-h", "--help"):
            print(__doc__)
            return 0
        else:
            print(f"Unknown argument: {args[i]}", file=sys.stderr)
            print(__doc__, file=sys.stderr)
            return 1

    cfg_dir = configs_dir()
    if not cfg_dir.exists():
        print(f"Error: esp32 configs not found at {cfg_dir}", file=sys.stderr)
        return 1

    # Resolve the authoritative source text.
    boards_text: str | None = None
    platform_text: str | None = None
    source_desc = ""

    env_boards = os.environ.get("FBUILD_ESP32_BOARDS_TXT")

    if download_version is not None:
        try:
            boards_text, platform_text = download_authoritative_text(download_version)
        except RuntimeError as exc:
            print(f"Error: {exc}", file=sys.stderr)
            return 1
        source_desc = f"pioarduino/arduino-esp32 release tag {download_version} (downloaded)"
    elif boards_txt_arg or env_boards:
        boards_path = Path(boards_txt_arg or env_boards or "")
        if not boards_path.exists():
            print(f"Error: boards.txt not found at {boards_path}", file=sys.stderr)
            return 1
        boards_text = boards_path.read_text(encoding="utf-8")
        platform_path = boards_path.with_name("platform.txt")
        platform_text = (
            platform_path.read_text(encoding="utf-8") if platform_path.exists() else ""
        )
        source_desc = f"explicit boards.txt: {boards_path}"
    else:
        found = find_cached_framework()
        if found is not None:
            boards_path, platform_path = found
            boards_text = boards_path.read_text(encoding="utf-8")
            platform_text = platform_path.read_text(encoding="utf-8")
            source_desc = f"installed framework: {boards_path}"

    if boards_text is None:
        print(
            "Error: could not locate an authoritative arduino-esp32 boards.txt.\n"
            "  Tried: --boards-txt, $FBUILD_ESP32_BOARDS_TXT, and the fbuild cache\n"
            "  (~/.fbuild/{dev,prod}/cache/platforms/framework-arduinoespressif32/).\n"
            "  Install the ESP32 framework, pass --boards-txt PATH, or use --download.",
            file=sys.stderr,
        )
        return 1

    platform_default = parse_platform_default(platform_text or "")
    boards_addrs = parse_boards_bootloader_addr(boards_text)
    known_chips = parse_known_chips(boards_text)

    print("Authoritative source: " + source_desc)
    print(f"platform.txt default build.bootloader_addr = {platform_default}")
    print(f"configs directory: {cfg_dir}")
    print()

    config_files = sorted(cfg_dir.glob("esp32*.json"))
    if not config_files:
        print(f"Error: no esp32*.json configs found in {cfg_dir}", file=sys.stderr)
        return 1

    header = f"{'CHIP':<12} {'CONFIG':<10} {'AUTHORITATIVE':<14} {'PART':<8} {'FW':<10} RESULT"
    print(header)
    print("-" * len(header))

    failures: list[str] = []

    for path in config_files:
        mcu, offsets = load_config_offsets(path)
        cfg_boot = offsets.get("bootloader")
        cfg_part = offsets.get("partitions")
        cfg_fw = offsets.get("firmware")

        chip_failures: list[str] = []

        auth = authoritative_offset(mcu, boards_addrs, known_chips, platform_default)
        auth_display = auth if auth is not None else "MISSING"

        if auth is None:
            chip_failures.append(
                f"{mcu}: chip is unknown to the authoritative boards.txt (no "
                f"`{mcu}.build.mcu` or `{mcu}.build.bootloader_addr` entry) -- "
                f"cannot verify this chip's bootloader offset"
            )
        elif cfg_boot is None:
            chip_failures.append(f"{mcu}: config has no esptool.flash_offsets.bootloader")
        elif normalize_offset(cfg_boot) != normalize_offset(auth):
            chip_failures.append(
                f"{mcu}: bootloader offset {cfg_boot!r} != authoritative {auth!r} "
                f"(boards.txt build.bootloader_addr)"
            )

        if cfg_part is None or normalize_offset(cfg_part) != normalize_offset(EXPECTED_PARTITIONS):
            chip_failures.append(
                f"{mcu}: partitions offset {cfg_part!r} != expected {EXPECTED_PARTITIONS!r}"
            )
        if cfg_fw is None or normalize_offset(cfg_fw) != normalize_offset(EXPECTED_FIRMWARE):
            chip_failures.append(
                f"{mcu}: firmware offset {cfg_fw!r} != expected {EXPECTED_FIRMWARE!r}"
            )

        result = "PASS" if not chip_failures else "FAIL"
        print(
            f"{mcu:<12} {str(cfg_boot):<10} {auth_display:<14} "
            f"{str(cfg_part):<8} {str(cfg_fw):<10} {result}"
        )
        failures.extend(chip_failures)

    print()
    if failures:
        print(f"FAILED: {len(failures)} flash-offset problem(s):")
        for f in failures:
            print(f"  - {f}")
        print()
        print(
            "The bootloader offset is a ROM-defined constant. The authoritative value is\n"
            "`<chip>.build.bootloader_addr` in arduino-esp32 boards.txt (ESP-IDF "
            "ESP_BOOTLOADER_OFFSET).\n"
            "Fix the config to match the authoritative source; do NOT guess."
        )
        return 1

    print(f"All {len(config_files)} esp32 config(s) match the authoritative flash offsets.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

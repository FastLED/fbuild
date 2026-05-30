#!/usr/bin/env python3
"""Fetch `build.extra_flags` from upstream PlatformIO board JSONs and patch
the missing fields in fbuild's bundled `assets/boards/json/*.json`.

Existing values are NEVER overwritten; only missing `extra_flags` entries
are added. Reads from a small set of upstream repos in priority order
(official platformio first, then known community forks).

Run from anywhere; resolves bundle path via `__file__`.

Usage:
    uv run python ci/enrich_extra_flags.py                # all platforms
    uv run python ci/enrich_extra_flags.py nordicnrf52    # one platform
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Optional

import urllib.request
import urllib.error

REPO_ROOT = Path(__file__).resolve().parent.parent
BUNDLE_DIR = REPO_ROOT / "crates" / "fbuild-config" / "assets" / "boards" / "json"

# (repo, branch) for each platform key — try in order, first hit wins.
# Some boards live in maxgerhardt forks rather than platformio main.
PLATFORM_SOURCES: dict[str, list[tuple[str, str]]] = {
    "nordicnrf52": [
        ("platformio/platform-nordicnrf52", "develop"),
        ("maxgerhardt/platform-nordicnrf52", "develop"),
    ],
    "raspberrypi": [
        ("platformio/platform-raspberrypi", "develop"),
        ("maxgerhardt/platform-raspberrypi", "develop"),
    ],
    "atmelavr": [("platformio/platform-atmelavr", "develop")],
    "atmelmegaavr": [("platformio/platform-atmelmegaavr", "develop")],
    "atmelsam": [("platformio/platform-atmelsam", "develop")],
    "espressif32": [
        ("platformio/platform-espressif32", "develop"),
        ("pioarduino/platform-espressif32", "develop"),
    ],
    "espressif8266": [("platformio/platform-espressif8266", "develop")],
    "ststm32": [("platformio/platform-ststm32", "develop")],
    "teensy": [("platformio/platform-teensy", "develop")],
    "nordicnrf51": [("platformio/platform-nordicnrf51", "develop")],
    "renesas-ra": [("platformio/platform-renesas-ra", "develop")],
    "siliconlabsefm32": [("platformio/platform-siliconlabsefm32", "develop")],
    "intel_arc32": [("platformio/platform-intel_arc32", "develop")],
}


def fetch_upstream_json(board_id: str, platform: str) -> Optional[dict]:
    """Try each source for `platform` until we find `boards/<board_id>.json`."""
    sources = PLATFORM_SOURCES.get(platform, [])
    for repo, branch in sources:
        url = f"https://raw.githubusercontent.com/{repo}/{branch}/boards/{board_id}.json"
        try:
            with urllib.request.urlopen(url, timeout=15) as resp:
                if resp.status == 200:
                    return json.loads(resp.read())
        except urllib.error.HTTPError as e:
            if e.code != 404:
                print(f"  ! {url}: HTTP {e.code}", file=sys.stderr)
        except Exception as e:
            print(f"  ! {url}: {e}", file=sys.stderr)
    return None


def patch_bundle(only_platform: Optional[str] = None) -> tuple[int, int, int]:
    """Walk bundle, fetch upstream for those missing extra_flags, patch in place.

    Returns (boards_examined, boards_patched, boards_skipped_no_upstream).
    """
    examined = 0
    patched = 0
    skipped_no_upstream = 0
    for path in sorted(BUNDLE_DIR.glob("*.json")):
        try:
            with path.open("r", encoding="utf-8") as f:
                data = json.load(f)
        except Exception as e:
            print(f"  ! {path.name}: parse fail: {e}", file=sys.stderr)
            continue

        build = data.get("build", {})
        if "extra_flags" in build:
            continue  # already present — never overwrite

        platform = data.get("platform", "")
        if only_platform and platform != only_platform:
            continue

        examined += 1
        board_id = data.get("id", path.stem)
        upstream = fetch_upstream_json(board_id, platform)
        if upstream is None:
            skipped_no_upstream += 1
            print(f"  - {board_id} ({platform}): no upstream JSON")
            continue

        upstream_extra = upstream.get("build", {}).get("extra_flags")
        if not upstream_extra:
            skipped_no_upstream += 1
            print(f"  - {board_id} ({platform}): upstream has no extra_flags")
            continue

        # Patch in place — insert extra_flags into existing build dict, keep
        # other fields and key order as much as possible. Re-serialize with
        # the same indentation FastLED's tests expect (2 spaces, sorted keys
        # NOT enforced — preserve original key order via dict insertion).
        new_build = {}
        # extra_flags goes near the top of build, after core/variant if present
        for key in build:
            new_build[key] = build[key]
            if key == "core" and "extra_flags" not in new_build:
                new_build["extra_flags"] = upstream_extra
        if "extra_flags" not in new_build:
            new_build["extra_flags"] = upstream_extra

        data["build"] = new_build
        with path.open("w", encoding="utf-8", newline="\n") as f:
            json.dump(data, f, indent=2)
            f.write("\n")
        patched += 1
        print(f"  + {board_id} ({platform}): {upstream_extra}")

    return examined, patched, skipped_no_upstream


def main() -> int:
    only = sys.argv[1] if len(sys.argv) > 1 else None
    if only:
        print(f"Enriching extra_flags for platform: {only}")
    else:
        print("Enriching extra_flags for ALL platforms")
    examined, patched, skipped = patch_bundle(only)
    print()
    print(f"Examined: {examined}, patched: {patched}, skipped (no upstream): {skipped}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

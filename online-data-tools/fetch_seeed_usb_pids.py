#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch Seeed Studio USB PID rows into merge_sources.py JSON.

The strongest public Seeed sources are the Seeed Boards Manager package index,
its current board-package archives, and Seeed's own ``platform-seeedboards``
board JSONs. Third-party Arduino cores and descriptors are emitted only as a
weak supplemental tier.

Output schema:
    {
      "2886:802f": {
        "vendor": "Seeed Technology Co., Ltd.",
        "product": "Seeeduino XIAO"
      }
    }
"""

from __future__ import annotations

import argparse
import io
import json
import re
import sys
import tarfile
import time
import urllib.parse
import urllib.request
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable, Literal

SEEED_VENDOR = "Seeed Technology Co., Ltd."
SEEED_VID = "2886"

PACKAGE_INDEX_URL = "https://files.seeedstudio.com/arduino/package_seeeduino_boards_index.json"
PACKAGE_ARCHITECTURES = {"samd", "nrf52", "mbed", "renesas_uno", "imxrt"}
PACKAGE_ARCH_ORDER = ("samd", "nrf52", "renesas_uno", "imxrt", "mbed")

SEEED_PLATFORM_TREE_URL = (
    "https://api.github.com/repos/Seeed-Studio/platform-seeedboards/git/trees/"
    "main?recursive=1"
)
SEEED_PLATFORM_RAW_BASE = (
    "https://raw.githubusercontent.com/Seeed-Studio/platform-seeedboards/main"
)

SUPPLEMENTAL_BOARD_SOURCES = (
    "https://raw.githubusercontent.com/espressif/arduino-esp32/master/boards.txt",
    "https://raw.githubusercontent.com/SiliconLabs/arduino/main/boards.txt",
)
ARDUINO_PICO_MAKEBOARDS_URL = (
    "https://raw.githubusercontent.com/earlephilhower/arduino-pico/master/"
    "tools/makeboards.py"
)
PLATFORMIO_ESPRESSIF_TREE_URL = (
    "https://api.github.com/repos/platformio/platform-espressif32/git/trees/"
    "develop?recursive=1"
)
PLATFORMIO_ESPRESSIF_RAW_BASE = (
    "https://raw.githubusercontent.com/platformio/platform-espressif32/develop"
)
CIRCUITPYTHON_TREE_URL = (
    "https://api.github.com/repos/adafruit/circuitpython/git/trees/main?recursive=1"
)
CIRCUITPYTHON_RAW_BASE = "https://raw.githubusercontent.com/adafruit/circuitpython/main"
TINYUF2_TREE_URL = "https://api.github.com/repos/adafruit/tinyuf2/git/trees/master?recursive=1"
TINYUF2_RAW_BASE = "https://raw.githubusercontent.com/adafruit/tinyuf2/master"

_C_DEFINE_RE = re.compile(
    r'^\s*#define\s+(?P<name>USB_(?:VID|PID|MANUFACTURER|PRODUCT)|PRODUCT_NAME)\s+'
    r'(?P<value>0x[0-9A-Fa-f]{4}|"[^"]+")\s*$',
    re.M,
)
_MAKE_RE = re.compile(
    r'^\s*(?P<name>USB_(?:VID|PID|MANUFACTURER|PRODUCT)|PRODUCT_NAME)\s*=\s*'
    r'(?P<value>0x[0-9A-Fa-f]{4}|"[^"]+")\s*$',
    re.M,
)
_MAKEBOARD_RE = re.compile(
    r'MakeBoard\(\s*"[^"]+"\s*,\s*"[^"]+"\s*,\s*"(?P<vendor>[^"]+)"\s*,'
    r'\s*"(?P<product>[^"]+)"\s*,\s*"(?P<vid>0x[0-9A-Fa-f]{4})"\s*,'
    r'\s*"(?P<pid>0x[0-9A-Fa-f]{4})"',
)


@dataclass(frozen=True)
class PackageSource:
    name: str
    architecture: str
    version: str
    url: str


@dataclass(frozen=True)
class TreeSource:
    name: str
    tree_url: str
    raw_base: str


def _fetch_text(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    last_error: Exception | None = None
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return resp.read().decode("utf-8", errors="replace")
        except Exception as e:
            last_error = e
            if attempt < 2:
                time.sleep(1 + attempt)
    assert last_error is not None
    raise last_error


def _fetch_bytes(url: str, *, timeout: float = 90.0) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    last_error: Exception | None = None
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return resp.read()
        except Exception as e:
            last_error = e
            if attempt < 2:
                time.sleep(1 + attempt)
    assert last_error is not None
    raise last_error


def _fetch_json(url: str) -> dict:
    return json.loads(_fetch_text(url))


def _hex4(value: str) -> str:
    text = value.strip().lower()
    if text.startswith("0x"):
        text = text[2:]
    return f"{int(text, 16):04x}"


def _string_value(value: str) -> str:
    value = value.strip()
    if value.startswith('"') and value.endswith('"'):
        return value[1:-1]
    return value


def _version_key(version: str) -> tuple[int, ...]:
    return tuple(int(part) for part in re.findall(r"\d+", version))


def _normalize_product_name(name: str) -> str:
    name = _string_value(name)
    name = re.sub(r"\s+", " ", name).strip()
    name = re.sub(r"\s+\(No Updates\)$", "", name)
    if "_" in name and " " not in name:
        name = name.replace("_", " ")
    if name.lower().startswith("xiao "):
        name = f"Seeed {name}"
    return name


def _full_product_name(manufacturer: str | None, product: str) -> str:
    product = _normalize_product_name(product)
    if not manufacturer:
        return product
    manufacturer = _normalize_product_name(manufacturer)
    if manufacturer.lower().startswith("seeed") and not product.lower().startswith("seeed"):
        return f"Seeed {product}"
    return product


def _collapse_products(names: Iterable[str]) -> str:
    unique = sorted(set(_normalize_product_name(name) for name in names if name.strip()))
    if len(unique) == 1:
        return unique[0]
    return " / ".join(unique)


def _merge_same_tier(
    base: dict[str, dict[str, str]],
    supplement: dict[str, dict[str, str]],
) -> dict[str, dict[str, str]]:
    out = dict(base)
    for key, value in sorted(supplement.items()):
        if key not in out:
            out[key] = value
            continue
        out[key] = {
            "vendor": out[key]["vendor"],
            "product": _collapse_products(
                [out[key]["product"], value["product"]],
            ),
        }
    return out


def _merge_fill_gaps(
    base: dict[str, dict[str, str]],
    supplement: dict[str, dict[str, str]],
) -> dict[str, dict[str, str]]:
    out = dict(base)
    for key, value in sorted(supplement.items()):
        out.setdefault(key, value)
    return out


def _append_value(
    values: dict[tuple[str, str], list[str]],
    board: str,
    path: str,
    value: str,
) -> None:
    values.setdefault((board, path), []).append(_hex4(value))


def _board_pid_path(key: str) -> tuple[str, str, str] | None:
    parts = key.split(".")
    if len(parts) < 3:
        return None
    board = parts[0]
    if len(parts) == 3 and parts[1] in {"vid", "pid"}:
        return board, f"direct:{parts[2]}", parts[1]
    if len(parts) == 4 and parts[1] == "upload_port" and parts[3] in {"vid", "pid"}:
        return board, f"upload_port:{parts[2]}", parts[3]
    if parts[1] in {"build", "upload"} and parts[2] in {"vid", "pid"}:
        index = parts[3] if len(parts) == 4 else "default"
        if len(parts) in {3, 4}:
            return board, f"{parts[1]}:{index}", parts[2]
    return None


def _pair_values(vids: list[str], pids: list[str]) -> list[tuple[str, str]]:
    if len(vids) == len(pids):
        return list(zip(vids, pids))
    if len(vids) == 1:
        return [(vids[0], pid) for pid in pids]
    if len(pids) == 1:
        return [(vid, pids[0]) for vid in vids]
    return [(vid, pid) for vid in vids for pid in pids]


def parse_boards_txt(text: str) -> dict[str, dict[str, str]]:
    """Parse Seeed VID rows from Arduino-style ``boards.txt``."""
    board_names: dict[str, str] = {}
    vids: dict[tuple[str, str], list[str]] = {}
    pids: dict[tuple[str, str], list[str]] = {}

    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip()
        value = value.strip()
        parts = key.split(".")
        if len(parts) == 2 and parts[1] == "name":
            board_names[parts[0]] = _normalize_product_name(value)
            continue
        parsed = _board_pid_path(key)
        if parsed is None:
            continue
        board, path, kind = parsed
        if kind == "vid":
            _append_value(vids, board, path, value)
        else:
            _append_value(pids, board, path, value)

    names_by_vidpid: dict[str, list[str]] = {}
    for key, pid_values in sorted(pids.items()):
        board, path = key
        vid_values = vids.get(key)
        if vid_values is None:
            continue
        product = board_names.get(board)
        if product is None:
            continue
        for vid, pid in _pair_values(vid_values, pid_values):
            if vid == SEEED_VID:
                names_by_vidpid.setdefault(f"{vid}:{pid}", []).append(product)

    return {
        vidpid: {
            "vendor": SEEED_VENDOR,
            "product": _collapse_products(names),
        }
        for vidpid, names in sorted(names_by_vidpid.items())
    }


def latest_package_sources(index_text: str) -> list[PackageSource]:
    payload = json.loads(index_text)
    latest: dict[str, PackageSource] = {}
    for package in payload.get("packages", []):
        for platform in package.get("platforms", []):
            architecture = platform.get("architecture")
            url = platform.get("url")
            version = platform.get("version")
            name = platform.get("name")
            if architecture not in PACKAGE_ARCHITECTURES:
                continue
            if not (
                isinstance(url, str)
                and url.startswith("https://files.seeedstudio.com/")
                and isinstance(version, str)
                and isinstance(name, str)
            ):
                continue
            source = PackageSource(name, architecture, version, url)
            if architecture not in latest or _version_key(version) > _version_key(
                latest[architecture].version,
            ):
                latest[architecture] = source
    return [latest[arch] for arch in PACKAGE_ARCH_ORDER if arch in latest]


def parse_package_archive(archive_bytes: bytes) -> dict[str, dict[str, str]]:
    entries: dict[str, dict[str, str]] = {}
    with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:*") as tf:
        for member in tf.getmembers():
            if not (member.isfile() and member.name.endswith("/boards.txt")):
                continue
            file_obj = tf.extractfile(member)
            if file_obj is None:
                continue
            text = file_obj.read().decode("utf-8", errors="replace")
            entries = _merge_same_tier(entries, parse_boards_txt(text))
    return dict(sorted(entries.items()))


def _tree_paths(tree_payload: dict) -> list[str]:
    tree = tree_payload.get("tree")
    if not isinstance(tree, list):
        return []
    paths = []
    for item in tree:
        if isinstance(item, dict) and isinstance(item.get("path"), str):
            paths.append(item["path"])
    return paths


def _raw_url(base: str, path: str) -> str:
    return f"{base}/{urllib.parse.quote(path, safe='/')}"


def _seeed_platform_paths(tree_payload: dict) -> list[str]:
    return [
        path
        for path in _tree_paths(tree_payload)
        if path.startswith("boards/") and path.endswith(".json") and "seeed" in path.lower()
    ]


def _skip_conflicting_seeed_platform_row(product: str, vid: str, pid: str) -> bool:
    _ = vid
    return "esp32c6" in product.lower() and pid in {"0046", "8046"}


def parse_board_json(
    text: str,
    *,
    skip_conflicts: bool,
) -> dict[str, dict[str, str]]:
    data = json.loads(text)
    name = data.get("name")
    if not isinstance(name, str) or not name.strip():
        return {}
    product = _normalize_product_name(name)
    hwids = data.get("build", {}).get("hwids")
    if not isinstance(hwids, list):
        return {}

    entries: dict[str, dict[str, str]] = {}
    for item in hwids:
        if not (isinstance(item, list) and len(item) == 2):
            continue
        vid, pid = item
        if not (isinstance(vid, str) and isinstance(pid, str)):
            continue
        vid_hex = _hex4(vid)
        pid_hex = _hex4(pid)
        if vid_hex != SEEED_VID:
            continue
        if skip_conflicts and _skip_conflicting_seeed_platform_row(product, vid_hex, pid_hex):
            continue
        entries[f"{vid_hex}:{pid_hex}"] = {
            "vendor": SEEED_VENDOR,
            "product": product,
        }
    return entries


def _parse_assignments(text: str, pattern: re.Pattern[str]) -> dict[str, str]:
    return {
        match.group("name"): _string_value(match.group("value"))
        for match in pattern.finditer(text)
    }


def parse_usb_descriptor_text(
    text: str,
    *,
    syntax: Literal["c", "make"],
) -> dict[str, dict[str, str]]:
    pattern = _C_DEFINE_RE if syntax == "c" else _MAKE_RE
    values = _parse_assignments(text, pattern)
    if "USB_VID" not in values or "USB_PID" not in values:
        return {}
    vid = _hex4(values["USB_VID"])
    if vid != SEEED_VID:
        return {}
    product = values.get("USB_PRODUCT") or values.get("PRODUCT_NAME")
    if not product:
        return {}
    manufacturer = values.get("USB_MANUFACTURER")
    return {
        f"{vid}:{_hex4(values['USB_PID'])}": {
            "vendor": SEEED_VENDOR,
            "product": _full_product_name(manufacturer, product),
        }
    }


def parse_arduino_pico_makeboards(text: str) -> dict[str, dict[str, str]]:
    entries: dict[str, dict[str, str]] = {}
    for match in _MAKEBOARD_RE.finditer(text):
        vendor = match.group("vendor")
        vid = _hex4(match.group("vid"))
        if vendor.lower() != "seeed" or vid != SEEED_VID:
            continue
        entries[f"{vid}:{_hex4(match.group('pid'))}"] = {
            "vendor": SEEED_VENDOR,
            "product": _normalize_product_name(match.group("product")),
        }
    return dict(sorted(entries.items()))


def _circuitpython_board_paths(tree_payload: dict) -> list[str]:
    return [
        path
        for path in _tree_paths(tree_payload)
        if path.endswith("/mpconfigboard.mk") and "/boards/seeed_" in path
    ]


def _tinyuf2_board_paths(tree_payload: dict) -> list[str]:
    return [
        path
        for path in _tree_paths(tree_payload)
        if path.endswith("/board.h") and "/boards/seeed_" in path
    ]


def _skip_descriptor_path(path: str) -> bool:
    # XIAO RP2040 uses the Raspberry Pi VID in first-party Seeed platform data.
    # Do not treat CircuitPython's 2886 row as a stronger Seeed allocation.
    return "seeed_xiao_rp2040" in path


def collect_seeed_packages(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_bytes: Callable[[str], bytes] = _fetch_bytes,
    index_url: str = PACKAGE_INDEX_URL,
) -> dict[str, dict[str, str]]:
    try:
        sources = latest_package_sources(fetch_text(index_url))
    except Exception as e:
        print(f"warning: Seeed package index fetch failed: {e}", file=sys.stderr)
        return {}

    entries: dict[str, dict[str, str]] = {}
    for source in sources:
        try:
            rows = parse_package_archive(fetch_bytes(source.url))
        except Exception as e:
            print(f"warning: {source.name} {source.version}: fetch failed: {e}", file=sys.stderr)
            continue
        entries = _merge_fill_gaps(entries, rows)
        print(
            f"{source.name} {source.version} ({source.architecture}): {len(rows)} PID(s)",
            file=sys.stderr,
        )
    return dict(sorted(entries.items()))


def collect_board_json_tree(
    *,
    source: TreeSource,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_json: Callable[[str], dict] = _fetch_json,
    skip_conflicts: bool,
) -> dict[str, dict[str, str]]:
    try:
        paths = _seeed_platform_paths(fetch_json(source.tree_url))
    except Exception as e:
        print(f"warning: {source.name}: tree fetch failed: {e}", file=sys.stderr)
        return {}

    entries: dict[str, dict[str, str]] = {}
    for path in paths:
        try:
            rows = parse_board_json(
                fetch_text(_raw_url(source.raw_base, path)),
                skip_conflicts=skip_conflicts,
            )
        except Exception as e:
            print(f"warning: {source.name}: {path}: fetch failed: {e}", file=sys.stderr)
            continue
        entries = _merge_same_tier(entries, rows)
    print(f"{source.name}: {len(entries)} PID(s)", file=sys.stderr)
    return dict(sorted(entries.items()))


def collect_descriptor_tree(
    *,
    name: str,
    tree_url: str,
    raw_base: str,
    syntax: Literal["c", "make"],
    path_filter: Callable[[dict], list[str]],
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_json: Callable[[str], dict] = _fetch_json,
) -> dict[str, dict[str, str]]:
    try:
        paths = path_filter(fetch_json(tree_url))
    except Exception as e:
        print(f"warning: {name}: tree fetch failed: {e}", file=sys.stderr)
        return {}

    entries: dict[str, dict[str, str]] = {}
    for path in paths:
        if _skip_descriptor_path(path):
            continue
        try:
            rows = parse_usb_descriptor_text(
                fetch_text(_raw_url(raw_base, path)),
                syntax=syntax,
            )
        except Exception as e:
            print(f"warning: {name}: {path}: fetch failed: {e}", file=sys.stderr)
            continue
        entries = _merge_same_tier(entries, rows)
    print(f"{name}: {len(entries)} PID(s)", file=sys.stderr)
    return dict(sorted(entries.items()))


def collect_first_party(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_bytes: Callable[[str], bytes] = _fetch_bytes,
    fetch_json: Callable[[str], dict] = _fetch_json,
) -> dict[str, dict[str, str]]:
    entries = collect_seeed_packages(fetch_text=fetch_text, fetch_bytes=fetch_bytes)
    entries = _merge_fill_gaps(
        entries,
        collect_board_json_tree(
            source=TreeSource(
                "Seeed platform-seeedboards",
                SEEED_PLATFORM_TREE_URL,
                SEEED_PLATFORM_RAW_BASE,
            ),
            fetch_text=fetch_text,
            fetch_json=fetch_json,
            skip_conflicts=True,
        ),
    )
    return dict(sorted(entries.items()))


def collect_supplemental(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_json: Callable[[str], dict] = _fetch_json,
) -> dict[str, dict[str, str]]:
    entries: dict[str, dict[str, str]] = {}
    for url in SUPPLEMENTAL_BOARD_SOURCES:
        try:
            rows = parse_boards_txt(fetch_text(url))
        except Exception as e:
            print(f"warning: supplemental boards {url}: fetch failed: {e}", file=sys.stderr)
            continue
        entries = _merge_same_tier(entries, rows)
        print(f"supplemental boards {url}: {len(rows)} PID(s)", file=sys.stderr)

    try:
        rows = parse_arduino_pico_makeboards(fetch_text(ARDUINO_PICO_MAKEBOARDS_URL))
    except Exception as e:
        print(f"warning: Arduino-Pico makeboards fetch failed: {e}", file=sys.stderr)
    else:
        entries = _merge_same_tier(entries, rows)
        print(f"Arduino-Pico Seeed rows: {len(rows)} PID(s)", file=sys.stderr)

    entries = _merge_same_tier(
        entries,
        collect_board_json_tree(
            source=TreeSource(
                "PlatformIO Espressif32",
                PLATFORMIO_ESPRESSIF_TREE_URL,
                PLATFORMIO_ESPRESSIF_RAW_BASE,
            ),
            fetch_text=fetch_text,
            fetch_json=fetch_json,
            skip_conflicts=False,
        ),
    )
    entries = _merge_same_tier(
        entries,
        collect_descriptor_tree(
            name="CircuitPython Seeed descriptors",
            tree_url=CIRCUITPYTHON_TREE_URL,
            raw_base=CIRCUITPYTHON_RAW_BASE,
            syntax="make",
            path_filter=_circuitpython_board_paths,
            fetch_text=fetch_text,
            fetch_json=fetch_json,
        ),
    )
    entries = _merge_same_tier(
        entries,
        collect_descriptor_tree(
            name="TinyUF2 Seeed descriptors",
            tree_url=TINYUF2_TREE_URL,
            raw_base=TINYUF2_RAW_BASE,
            syntax="c",
            path_filter=_tinyuf2_board_paths,
            fetch_text=fetch_text,
            fetch_json=fetch_json,
        ),
    )
    return dict(sorted(entries.items()))


def collect(
    *,
    tier: Literal["first-party", "supplemental", "all"] = "all",
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_bytes: Callable[[str], bytes] = _fetch_bytes,
    fetch_json: Callable[[str], dict] = _fetch_json,
) -> dict[str, dict[str, str]]:
    if tier == "first-party":
        return collect_first_party(
            fetch_text=fetch_text,
            fetch_bytes=fetch_bytes,
            fetch_json=fetch_json,
        )
    if tier == "supplemental":
        return collect_supplemental(fetch_text=fetch_text, fetch_json=fetch_json)
    first_party = collect_first_party(
        fetch_text=fetch_text,
        fetch_bytes=fetch_bytes,
        fetch_json=fetch_json,
    )
    supplemental = collect_supplemental(fetch_text=fetch_text, fetch_json=fetch_json)
    return dict(sorted(_merge_fill_gaps(first_party, supplemental).items()))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--tier",
        choices=("first-party", "supplemental", "all"),
        default="all",
        help="Source priority tier to emit.",
    )
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()

    entries = collect(tier=args.tier)
    args.out.write_text(
        json.dumps(OrderedDict(sorted(entries.items())), indent=2, ensure_ascii=False)
        + "\n",
        encoding="utf-8",
    )
    print(f"wrote {args.out}: {len(entries)} Seeed PID(s) [{args.tier}]")
    return 0


if __name__ == "__main__":
    sys.exit(main())

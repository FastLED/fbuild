#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch SparkFun USB PID rows into merge_sources.py JSON.

SparkFun does not publish a single public USB PID registry. This script treats
SparkFun-maintained board packages and product descriptors as the strongest
public sources for SparkFun VID ``0x1b4f`` rows, and treats third-party package
metadata as a weak gap-filling tier.

Output schema:
    {
      "1b4f:9206": {
        "vendor": "SparkFun",
        "product": "SparkFun Pro Micro ATmega32U4 (5V, 16 MHz)"
      }
    }
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.parse
import urllib.request
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable, Literal

SPARKFUN_VENDOR = "SparkFun"
SPARKFUN_VID = "1b4f"


@dataclass(frozen=True)
class TextSource:
    name: str
    url: str


@dataclass(frozen=True)
class PlatformIoRepo:
    name: str
    tree_url: str
    raw_base: str


FIRST_PARTY_BOARD_SOURCES = (
    TextSource(
        "SparkFun Arduino AVR",
        "https://raw.githubusercontent.com/sparkfun/Arduino_Boards/main/"
        "sparkfun/avr/boards.txt",
    ),
    TextSource(
        "SparkFun Arduino SAMD",
        "https://raw.githubusercontent.com/sparkfun/Arduino_Boards/main/"
        "sparkfun/samd/boards.txt",
    ),
    TextSource(
        "SparkFun Arduino Apollo3",
        "https://raw.githubusercontent.com/sparkfun/Arduino_Apollo3/v1.2.1/"
        "boards.txt",
    ),
    TextSource(
        "SparkFun Pro nRF52840 Mini",
        "https://raw.githubusercontent.com/sparkfun/nRF52840_Breakout_MDBT50Q/"
        "master/Firmware/Arduino/sparkfun_boards.txt",
    ),
    TextSource(
        "SparkFun Pro Micro ESP32-C3",
        "https://raw.githubusercontent.com/sparkfun/SparkFun_Pro_Micro-ESP32C3/"
        "main/Arduino_Board_Files/sparkfun_boards.txt",
    ),
    TextSource(
        "SparkFun Qwiic Micro SAMD21E",
        "https://raw.githubusercontent.com/sparkfun/SparkFun_Qwiic_Micro_SAMD21E/"
        "main/Firmware/Arduino_Board_Files/boards.txt",
    ),
    TextSource(
        "SparkFun MicroMod SAMD51",
        "https://raw.githubusercontent.com/sparkfun/MicroMod_Processor_Board-SAMD51/"
        "master/Arduino%20Board%20Files/boards.txt",
    ),
)

FIRST_PARTY_DESCRIPTOR_SOURCES = (
    TextSource(
        "SparkFun SAMD51 Thing Plus UF2",
        "https://raw.githubusercontent.com/sparkfun/SAMD51_Thing_Plus/master/"
        "uf2-bootloader/sparkfun-samd51-thingplus/board_config.h",
    ),
    TextSource(
        "SparkFun MicroMod SAMD51 UF2",
        "https://raw.githubusercontent.com/sparkfun/MicroMod_Processor_Board-SAMD51/"
        "master/uf2-bootloader/sparkfun-samd51-micromod/board_config.h",
    ),
    TextSource(
        "SparkFun Qwiic Micro CircuitPython no-flash",
        "https://raw.githubusercontent.com/sparkfun/SparkFun_Qwiic_Micro_SAMD21E/"
        "main/Firmware/Circuit%20Python%20Build%20Files/"
        "sparkfun_qwiic_micro_no_flash/mpconfigboard.mk",
    ),
    TextSource(
        "SparkFun Qwiic Micro CircuitPython with-flash",
        "https://raw.githubusercontent.com/sparkfun/SparkFun_Qwiic_Micro_SAMD21E/"
        "main/Firmware/Circuit%20Python%20Build%20Files/"
        "sparkfun_qwiic_micro_with_flash/mpconfigboard.mk",
    ),
)

PLATFORMIO_REPOS = (
    PlatformIoRepo(
        "PlatformIO Atmel AVR",
        "https://api.github.com/repos/platformio/platform-atmelavr/git/trees/"
        "develop?recursive=1",
        "https://raw.githubusercontent.com/platformio/platform-atmelavr/develop",
    ),
    PlatformIoRepo(
        "PlatformIO Atmel SAM",
        "https://api.github.com/repos/platformio/platform-atmelsam/git/trees/"
        "develop?recursive=1",
        "https://raw.githubusercontent.com/platformio/platform-atmelsam/develop",
    ),
    PlatformIoRepo(
        "PlatformIO Espressif32",
        "https://api.github.com/repos/platformio/platform-espressif32/git/trees/"
        "develop?recursive=1",
        "https://raw.githubusercontent.com/platformio/platform-espressif32/develop",
    ),
    PlatformIoRepo(
        "PlatformIO Nordic nRF52",
        "https://api.github.com/repos/platformio/platform-nordicnrf52/git/trees/"
        "develop?recursive=1",
        "https://raw.githubusercontent.com/platformio/platform-nordicnrf52/develop",
    ),
)

CIRCUITPYTHON_TREE_URL = (
    "https://api.github.com/repos/adafruit/circuitpython/git/trees/main?recursive=1"
)
CIRCUITPYTHON_RAW_BASE = "https://raw.githubusercontent.com/adafruit/circuitpython/main"

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


def _fetch_text(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


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


def _normalize_product_name(name: str) -> str:
    return re.sub(r"\s+", " ", name).strip()


def _full_product_name(manufacturer: str | None, product: str) -> str:
    product = _normalize_product_name(product)
    if not manufacturer:
        return product
    manufacturer = _normalize_product_name(manufacturer)
    if not manufacturer.lower().startswith("sparkfun"):
        return product
    if product.lower().startswith("sparkfun"):
        return product
    return f"{SPARKFUN_VENDOR} {product}"


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
        return board, f"upload:{parts[2]}", parts[3]
    if parts[1] == "build" and parts[2] in {"vid", "pid"}:
        index = parts[3] if len(parts) == 4 else "default"
        if len(parts) in {3, 4}:
            return board, f"build:{index}", parts[2]
    if (
        len(parts) in {6, 7}
        and parts[1] == "menu"
        and parts[4] == "build"
        and parts[5] in {"vid", "pid"}
    ):
        index = parts[6] if len(parts) == 7 else "default"
        return board, f"menu:{parts[2]}:{parts[3]}:{index}", parts[5]
    return None


def _pair_values(vids: list[str], pids: list[str]) -> list[tuple[str, str]]:
    if len(vids) == len(pids):
        return list(zip(vids, pids))
    if len(vids) == 1:
        return [(vids[0], pid) for pid in pids]
    if len(pids) == 1:
        return [(vid, pids[0]) for vid in vids]
    return [(vid, pid) for vid in vids for pid in pids]


def _menu_product_name(
    board: str,
    board_name: str,
    menu_labels: dict[tuple[str, str, str], str],
    path: str,
) -> str:
    _kind, menu, option, _index = path.split(":", 3)
    label = menu_labels.get((board, menu, option))
    if not label:
        return board_name
    return f"{board_name} {_normalize_product_name(label)}"


def parse_boards_txt(text: str) -> dict[str, dict[str, str]]:
    """Parse SparkFun VID rows from Arduino-style ``boards.txt``."""
    board_names: dict[str, str] = {}
    board_products: dict[str, str] = {}
    menu_labels: dict[tuple[str, str, str], str] = {}
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
        board = parts[0]
        if len(parts) == 2 and parts[1] == "name":
            board_names[board] = _normalize_product_name(value)
            continue
        if len(parts) == 3 and parts[1:] == ["build", "usb_product"]:
            board_products[board] = _normalize_product_name(_string_value(value))
            continue
        if len(parts) == 4 and parts[1] == "menu":
            menu_labels[(board, parts[2], parts[3])] = _normalize_product_name(value)
            continue

        parsed = _board_pid_path(key)
        if parsed is None:
            continue
        parsed_board, path, kind = parsed
        if kind == "vid":
            _append_value(vids, parsed_board, path, value)
        else:
            _append_value(pids, parsed_board, path, value)

    names_by_vidpid: dict[str, list[str]] = {}
    for key, pid_values in sorted(pids.items()):
        board, path = key
        vid_values = vids.get(key)
        if vid_values is None and path.startswith("menu:"):
            vid_values = vids.get((board, "build:default"))
        if vid_values is None:
            continue

        product = board_products.get(board) or board_names.get(board)
        if product is None:
            continue
        if path.startswith("menu:"):
            product = _menu_product_name(board, product, menu_labels, path)

        for vid, pid in _pair_values(vid_values, pid_values):
            if vid == SPARKFUN_VID:
                names_by_vidpid.setdefault(f"{vid}:{pid}", []).append(product)

    return {
        vidpid: {
            "vendor": SPARKFUN_VENDOR,
            "product": _collapse_products(names),
        }
        for vidpid, names in sorted(names_by_vidpid.items())
    }


def _parse_assignments(text: str, pattern: re.Pattern[str]) -> dict[str, str]:
    return {
        match.group("name"): _string_value(match.group("value"))
        for match in pattern.finditer(text)
    }


def parse_usb_descriptor_text(text: str, *, syntax: Literal["c", "make"]) -> dict[str, dict[str, str]]:
    """Parse USB rows from UF2 ``board_config.h`` or CircuitPython make files."""
    pattern = _C_DEFINE_RE if syntax == "c" else _MAKE_RE
    values = _parse_assignments(text, pattern)
    if "USB_VID" not in values or "USB_PID" not in values:
        return {}

    vid = _hex4(values["USB_VID"])
    if vid != SPARKFUN_VID:
        return {}
    product = values.get("USB_PRODUCT") or values.get("PRODUCT_NAME")
    if not product:
        return {}
    manufacturer = values.get("USB_MANUFACTURER")
    return {
        f"{vid}:{_hex4(values['USB_PID'])}": {
            "vendor": SPARKFUN_VENDOR,
            "product": _full_product_name(manufacturer, product),
        }
    }


def parse_platformio_board_json(text: str) -> dict[str, dict[str, str]]:
    """Parse PlatformIO ``build.hwids`` rows for SparkFun VID devices."""
    data = json.loads(text)
    name = data.get("name")
    if not isinstance(name, str) or not name.strip():
        return {}
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
        if vid_hex != SPARKFUN_VID:
            continue
        entries[f"{vid_hex}:{_hex4(pid)}"] = {
            "vendor": SPARKFUN_VENDOR,
            "product": _normalize_product_name(name),
        }
    return entries


def _tree_paths(tree_payload: dict) -> list[str]:
    tree = tree_payload.get("tree")
    if not isinstance(tree, list):
        return []
    paths = []
    for item in tree:
        if isinstance(item, dict) and isinstance(item.get("path"), str):
            paths.append(item["path"])
    return paths


def _platformio_board_paths(tree_payload: dict) -> list[str]:
    return [
        path
        for path in _tree_paths(tree_payload)
        if path.startswith("boards/")
        and path.endswith(".json")
        and "sparkfun" in Path(path).name.lower()
    ]


def _circuitpython_board_paths(tree_payload: dict) -> list[str]:
    return [
        path
        for path in _tree_paths(tree_payload)
        if path.endswith("/mpconfigboard.mk") and "/boards/sparkfun_" in path
    ]


def _raw_url(base: str, path: str) -> str:
    return f"{base}/{urllib.parse.quote(path, safe='/')}"


def collect_first_party(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    board_sources: Iterable[TextSource] = FIRST_PARTY_BOARD_SOURCES,
    descriptor_sources: Iterable[TextSource] = FIRST_PARTY_DESCRIPTOR_SOURCES,
) -> dict[str, dict[str, str]]:
    entries: dict[str, dict[str, str]] = {}
    for source in board_sources:
        try:
            rows = parse_boards_txt(fetch_text(source.url))
        except Exception as e:
            print(f"warning: {source.name}: {source.url}: fetch failed: {e}", file=sys.stderr)
            continue
        entries = _merge_same_tier(entries, rows)
        print(f"{source.name}: {len(rows)} PID(s)", file=sys.stderr)

    for source in descriptor_sources:
        syntax: Literal["c", "make"] = "make" if source.url.endswith(".mk") else "c"
        try:
            rows = parse_usb_descriptor_text(fetch_text(source.url), syntax=syntax)
        except Exception as e:
            print(f"warning: {source.name}: {source.url}: fetch failed: {e}", file=sys.stderr)
            continue
        entries = _merge_same_tier(entries, rows)
        print(f"{source.name}: {len(rows)} PID(s)", file=sys.stderr)
    return dict(sorted(entries.items()))


def collect_platformio(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_json: Callable[[str], dict] = _fetch_json,
    repos: Iterable[PlatformIoRepo] = PLATFORMIO_REPOS,
) -> dict[str, dict[str, str]]:
    entries: dict[str, dict[str, str]] = {}
    for repo in repos:
        try:
            paths = _platformio_board_paths(fetch_json(repo.tree_url))
        except Exception as e:
            print(f"warning: {repo.name}: tree fetch failed: {e}", file=sys.stderr)
            continue

        repo_entries: dict[str, dict[str, str]] = {}
        for path in paths:
            try:
                rows = parse_platformio_board_json(fetch_text(_raw_url(repo.raw_base, path)))
            except Exception as e:
                print(f"warning: {repo.name}: {path}: fetch failed: {e}", file=sys.stderr)
                continue
            repo_entries = _merge_same_tier(repo_entries, rows)
        entries = _merge_same_tier(entries, repo_entries)
        print(f"{repo.name}: {len(repo_entries)} PID(s)", file=sys.stderr)
    return dict(sorted(entries.items()))


def collect_circuitpython(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_json: Callable[[str], dict] = _fetch_json,
    tree_url: str = CIRCUITPYTHON_TREE_URL,
    raw_base: str = CIRCUITPYTHON_RAW_BASE,
) -> dict[str, dict[str, str]]:
    try:
        paths = _circuitpython_board_paths(fetch_json(tree_url))
    except Exception as e:
        print(f"warning: CircuitPython tree fetch failed: {e}", file=sys.stderr)
        return {}

    entries: dict[str, dict[str, str]] = {}
    for path in paths:
        try:
            rows = parse_usb_descriptor_text(
                fetch_text(_raw_url(raw_base, path)),
                syntax="make",
            )
        except Exception as e:
            print(f"warning: CircuitPython {path}: fetch failed: {e}", file=sys.stderr)
            continue
        entries = _merge_same_tier(entries, rows)
    print(f"CircuitPython SparkFun descriptors: {len(entries)} PID(s)", file=sys.stderr)
    return dict(sorted(entries.items()))


def collect_supplemental(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_json: Callable[[str], dict] = _fetch_json,
) -> dict[str, dict[str, str]]:
    entries = collect_platformio(fetch_text=fetch_text, fetch_json=fetch_json)
    entries = _merge_same_tier(
        entries,
        collect_circuitpython(fetch_text=fetch_text, fetch_json=fetch_json),
    )
    return dict(sorted(entries.items()))


def collect(
    *,
    tier: Literal["first-party", "supplemental", "all"] = "all",
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_json: Callable[[str], dict] = _fetch_json,
) -> dict[str, dict[str, str]]:
    if tier == "first-party":
        return collect_first_party(fetch_text=fetch_text)
    if tier == "supplemental":
        return collect_supplemental(fetch_text=fetch_text, fetch_json=fetch_json)
    first_party = collect_first_party(fetch_text=fetch_text)
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
    print(f"wrote {args.out}: {len(entries)} SparkFun PID(s) [{args.tier}]")
    return 0


if __name__ == "__main__":
    sys.exit(main())

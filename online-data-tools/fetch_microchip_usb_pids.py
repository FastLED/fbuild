#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""Fetch Microchip / Atmel USB PID rows into merge_sources.py JSON.

Microchip does not publish a single public USB PID registry. This script keeps
source priority explicit:

* ``--tier first-party`` emits rows from Microchip-maintained pyedbglib and
  pykitinfo.
* ``--tier supplemental`` emits weaker fill-gap rows from AVRDUDE and selected
  third-party Arduino board packages. These rows should be merged after
  first-party and generic USB-ID sources.
* ``--tier all`` is mainly for local inspection/tests; first-party rows win
  over supplemental rows for duplicate VID:PIDs.

Output schema:
    {
      "03eb:2141": {
        "vendor": "Atmel Corp.",
        "product": "Atmel-ICE"
      }
    }
"""

from __future__ import annotations

import argparse
import ast
import io
import json
import re
import sys
import urllib.request
import zipfile
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable, Literal

ATMEL_VENDOR = "Atmel Corp."
MICROCHIP_VENDOR = "Microchip Technology, Inc."
ATMEL_VID = "03eb"
MICROCHIP_VID = "04d8"

PYEDBGLIB_TOOLINFO_URL = (
    "https://raw.githubusercontent.com/microchip-pic-avr-tools/pyedbglib/"
    "main/pyedbglib/hidtransport/toolinfo.py"
)
PYKITINFO_TOOLS_URL = (
    "https://raw.githubusercontent.com/microchip-pic-avr-tools/pykitinfo/"
    "main/pykitinfo/tools.py"
)
AVRDUDE_USBDEVS_URL = (
    "https://raw.githubusercontent.com/avrdudes/avrdude/main/src/usbdevs.h"
)
ARDUINO_SAMD_BOARDS_URL = (
    "https://raw.githubusercontent.com/arduino/ArduinoCore-samd/master/boards.txt"
)
ARDUINO_MEGAAVR_BOARDS_URL = (
    "https://raw.githubusercontent.com/arduino/ArduinoCore-megaavr/master/boards.txt"
)
LOWPOWERLAB_PACKAGE_INDEX_URL = (
    "https://lowpowerlab.github.io/MoteinoCore/package_LowPowerLab_index.json"
)

PYEDBGLIB_PRODUCTS = {
    "USB_TOOL_DEVICE_PRODUCT_ID_JTAGICE3": "JTAGICE3",
    "USB_TOOL_DEVICE_PRODUCT_ID_ATMELICE": "Atmel-ICE",
    "USB_TOOL_DEVICE_PRODUCT_ID_POWERDEBUGGER": "Power Debugger",
    "USB_TOOL_DEVICE_PRODUCT_ID_EDBG_A": "EDBG",
    "USB_TOOL_DEVICE_PRODUCT_ID_MSD": "EDBG MSD interface",
    "USB_TOOL_DEVICE_PRODUCT_ID_ZERO": "Arduino Zero EDBG",
    "USB_TOOL_DEVICE_PRODUCT_ID_PUBLIC_EDBG_C": "Public EDBG CMSIS-DAP",
    "USB_TOOL_DEVICE_PRODUCT_ID_KRAKEN": "Kraken CMSIS-DAP",
    "USB_TOOL_DEVICE_PRODUCT_ID_MEDBG": "mEDBG",
    "USB_TOOL_DEVICE_PRODUCT_ID_NEDBG_HID_MSD_DGI_CDC": "nEDBG",
    "USB_TOOL_DEVICE_PRODUCT_ID_PICKIT4_HID_CDC": "MPLAB PICkit 4",
    "USB_TOOL_DEVICE_PRODUCT_ID_SNAP_HID_CDC": "MPLAB Snap",
    "USB_TOOL_DEVICE_PRODUCT_ID_ICD4_HID_CDC": "MPLAB ICD 4",
    "USB_TOOL_DEVICE_PRODUCT_ID_ICE4_HID_CDC": "MPLAB ICE 4",
}


@dataclass(frozen=True)
class AvrdudeEntry:
    symbol: str
    vendor_symbol: str
    product: str


AVRDUDE_PRODUCTS = (
    AvrdudeEntry("USB_DEVICE_JTAGICEMKII", "USB_VENDOR_ATMEL", "AVR JTAGICE mkII"),
    AvrdudeEntry("USB_DEVICE_AVRISPMKII", "USB_VENDOR_ATMEL", "AVR ISP mkII"),
    AvrdudeEntry("USB_DEVICE_STK600", "USB_VENDOR_ATMEL", "STK600 development board"),
    AvrdudeEntry("USB_DEVICE_AVRDRAGON", "USB_VENDOR_ATMEL", "AVR Dragon"),
    AvrdudeEntry("USB_DEVICE_JTAGICE3", "USB_VENDOR_ATMEL", "AVR JTAGICE3"),
    AvrdudeEntry(
        "USB_DEVICE_XPLAINEDPRO",
        "USB_VENDOR_ATMEL",
        "Xplained Pro board debugger",
    ),
    AvrdudeEntry("USB_DEVICE_JTAG3_EDBG", "USB_VENDOR_ATMEL", "JTAGICE3/EDBG"),
    AvrdudeEntry("USB_DEVICE_ATMEL_ICE", "USB_VENDOR_ATMEL", "Atmel-ICE"),
    AvrdudeEntry("USB_DEVICE_POWERDEBUGGER", "USB_VENDOR_ATMEL", "Power Debugger"),
    AvrdudeEntry("USB_DEVICE_XPLAINEDMINI", "USB_VENDOR_ATMEL", "Xplained Mini mEDBG"),
    AvrdudeEntry("USB_DEVICE_PKOBN", "USB_VENDOR_ATMEL", "nEDBG"),
    AvrdudeEntry("USB_DEVICE_PICKIT4_AVR_MODE", "USB_VENDOR_ATMEL", "MPLAB PICkit 4"),
    AvrdudeEntry("USB_DEVICE_SNAP_AVR_MODE", "USB_VENDOR_ATMEL", "MPLAB Snap"),
    AvrdudeEntry("USB_DEVICE_PICKIT4_PIC_MODE", "USB_VENDOR_MICROCHIP", "MPLAB PICkit 4"),
    AvrdudeEntry(
        "USB_DEVICE_PICKIT4_PIC_MODE_BL",
        "USB_VENDOR_MICROCHIP",
        "MPLAB PICkit 4 Bootloader",
    ),
    AvrdudeEntry("USB_DEVICE_PICKIT5", "USB_VENDOR_MICROCHIP", "MPLAB PICkit 5"),
    AvrdudeEntry("USB_DEVICE_SNAP_PIC_MODE", "USB_VENDOR_MICROCHIP", "MPLAB Snap"),
    AvrdudeEntry(
        "USB_DEVICE_SNAP_PIC_MODE_BL",
        "USB_VENDOR_MICROCHIP",
        "MPLAB Snap Bootloader",
    ),
    AvrdudeEntry(
        "USB_DEVICE_PICKIT_BASIC",
        "USB_VENDOR_MICROCHIP",
        "MPLAB PICkit Basic",
    ),
    AvrdudeEntry(
        "USB_DEVICE_PICKIT_BASIC_CDC",
        "USB_VENDOR_MICROCHIP",
        "MPLAB PICkit Basic",
    ),
    AvrdudeEntry(
        "USB_DEVICE_PICKIT_BASIC_CDC_VENDOR",
        "USB_VENDOR_MICROCHIP",
        "MPLAB PICkit Basic",
    ),
    AvrdudeEntry(
        "USB_DEVICE_PICKIT_BASIC_BL",
        "USB_VENDOR_MICROCHIP",
        "MPLAB PICkit Basic Bootloader",
    ),
    AvrdudeEntry(
        "USB_DEVICE_PICKIT_BASIC_CIMSIS",
        "USB_VENDOR_MICROCHIP",
        "MPLAB PICkit Basic CMSIS-DAP",
    ),
    AvrdudeEntry(
        "USB_DEVICE_PICKIT_BASIC_CIMSIS_CDC",
        "USB_VENDOR_MICROCHIP",
        "MPLAB PICkit Basic CMSIS-DAP",
    ),
    AvrdudeEntry(
        "USB_DEVICE_PICKIT_BASIC_CIMSIS_DGI",
        "USB_VENDOR_MICROCHIP",
        "MPLAB PICkit Basic CMSIS-DAP",
    ),
    AvrdudeEntry(
        "USB_DEVICE_PICKIT_BASIC_CIMSIS_MSD",
        "USB_VENDOR_MICROCHIP",
        "MPLAB PICkit Basic CMSIS-DAP",
    ),
    AvrdudeEntry("USBASP_OLD_PID", "USBASP_OLD_VID", "USBasp"),
)

LOWPOWERLAB_ALLOWED_NAMES = {"CurrentRanger", "Moteino M0"}

_HEX_DEFINE_RE = re.compile(
    r"^#define\s+(?P<name>[A-Z0-9_]+)\s+0x(?P<value>[0-9A-Fa-f]{4})",
    re.M,
)
_PY_CONST_RE = re.compile(
    r"^(?P<name>USB_[A-Z0-9_]+)\s*=\s*0x(?P<value>[0-9A-Fa-f]{4})",
    re.M,
)
_BOARD_NAME_RE = re.compile(r"^(?P<board>[A-Za-z0-9_]+)\.name=(?P<name>.+)$")
_BOARD_VID_RE = re.compile(
    r"^(?P<board>[A-Za-z0-9_]+)\.(?:vid\.(?P<index_a>\d+)|"
    r"upload_port\.(?P<index_b>\d+)\.vid)="
    r"0x(?P<vid>[0-9A-Fa-f]{4})$"
)
_BOARD_PID_RE = re.compile(
    r"^(?P<board>[A-Za-z0-9_]+)\.(?:pid\.(?P<index_a>\d+)|"
    r"upload_port\.(?P<index_b>\d+)\.pid)="
    r"0x(?P<pid>[0-9A-Fa-f]{4})$"
)


def _fetch_text(url: str, *, timeout: float = 30.0) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read().decode("utf-8", errors="replace")


def _fetch_bytes(url: str, *, timeout: float = 30.0) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "fbuild-bot/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read()


def _vendor_for_vid(vid: str) -> str:
    if vid == ATMEL_VID:
        return ATMEL_VENDOR
    if vid == MICROCHIP_VID:
        return MICROCHIP_VENDOR
    raise ValueError(f"unexpected Microchip/Atmel VID: {vid}")


def _to_entry_map(pid_to_product: dict[str, str], *, vid: str) -> dict[str, dict[str, str]]:
    return {
        f"{vid}:{pid}": {"vendor": _vendor_for_vid(vid), "product": product}
        for pid, product in sorted(pid_to_product.items())
    }


def _merge_fill_gaps(
    base: dict[str, dict[str, str]],
    supplement: dict[str, dict[str, str]],
) -> dict[str, dict[str, str]]:
    out = dict(base)
    for key, value in sorted(supplement.items()):
        out.setdefault(key, value)
    return out


def _normalize_product_name(name: str) -> str:
    out = name.replace("\u00ae", "").replace("\u2122", "")
    out = re.sub(r"\bPICkit\s*(\d)", r"PICkit \1", out)
    out = re.sub(r"\bICD\s*(\d)\b", r"ICD \1", out)
    out = re.sub(r"\bICE\s*(\d)\b", r"ICE \1", out)
    out = re.sub(r"\s+", " ", out)
    return out.strip()


def parse_pyedbglib_toolinfo(text: str) -> dict[str, str]:
    """Parse first-party Atmel VID debugger rows from pyedbglib."""
    constants = {
        match.group("name"): match.group("value").lower()
        for match in _PY_CONST_RE.finditer(text)
    }
    if constants.get("USB_VID_ATMEL") != ATMEL_VID:
        raise ValueError("pyedbglib toolinfo.py does not declare USB_VID_ATMEL 0x03eb")

    missing = sorted(set(PYEDBGLIB_PRODUCTS) - set(constants))
    if missing:
        raise ValueError(f"pyedbglib missing expected product constants: {missing}")

    pid_to_product: dict[str, str] = {}
    for symbol, product in PYEDBGLIB_PRODUCTS.items():
        pid = constants[symbol]
        previous = pid_to_product.get(pid)
        if previous is not None and previous != product:
            raise ValueError(f"duplicate pyedbglib PID 0x{pid}: {previous!r} vs {product!r}")
        pid_to_product[pid] = product
    return dict(sorted(pid_to_product.items()))


def _literal_with_names(node: ast.AST, names: dict[str, object]) -> object:
    if isinstance(node, ast.Constant):
        return node.value
    if isinstance(node, ast.Name):
        return names[node.id]
    if isinstance(node, ast.List):
        return [_literal_with_names(item, names) for item in node.elts]
    if isinstance(node, ast.Tuple):
        return tuple(_literal_with_names(item, names) for item in node.elts)
    if isinstance(node, ast.Dict):
        return {
            _literal_with_names(key, names): _literal_with_names(value, names)
            for key, value in zip(node.keys, node.values)
            if key is not None
        }
    raise ValueError(f"unsupported Python literal node: {type(node).__name__}")


def parse_pykitinfo_tools(text: str) -> dict[str, str]:
    """Parse first-party Microchip VID non-HID tool rows from pykitinfo."""
    module = ast.parse(text)
    names: dict[str, object] = {}
    tools_node: ast.AST | None = None
    for statement in module.body:
        if not isinstance(statement, ast.Assign) or len(statement.targets) != 1:
            continue
        target = statement.targets[0]
        if not isinstance(target, ast.Name):
            continue
        if target.id == "MICROCHIP_NON_HID_TOOLS":
            tools_node = statement.value
            continue
        try:
            names[target.id] = _literal_with_names(statement.value, names)
        except (KeyError, ValueError):
            continue

    if names.get("MICROCHIP_VID") != int(MICROCHIP_VID, 16):
        raise ValueError("pykitinfo tools.py does not declare MICROCHIP_VID 0x04d8")
    if tools_node is None:
        raise ValueError("pykitinfo tools.py does not declare MICROCHIP_NON_HID_TOOLS")

    raw_tools = _literal_with_names(tools_node, names)
    if not isinstance(raw_tools, list):
        raise ValueError("MICROCHIP_NON_HID_TOOLS is not a list")

    pid_to_product: dict[str, str] = {}
    for tool in raw_tools:
        if not isinstance(tool, dict):
            continue
        if tool.get("VID") != int(MICROCHIP_VID, 16):
            continue
        pid = f"{int(tool['PID']):04x}"
        product = _normalize_product_name(str(tool["Name"]))
        if tool.get("Serial port") is True and "CDC" not in product:
            product = f"{product} CDC"
        previous = pid_to_product.get(pid)
        if previous is not None and previous != product:
            raise ValueError(f"duplicate pykitinfo PID 0x{pid}: {previous!r} vs {product!r}")
        pid_to_product[pid] = product
    return dict(sorted(pid_to_product.items()))


def parse_avrdude_usbdevs(text: str) -> dict[str, dict[str, str]]:
    """Parse AVRDUDE USB device defines into merge-compatible entries."""
    defines = {
        match.group("name"): match.group("value").lower()
        for match in _HEX_DEFINE_RE.finditer(text)
    }
    if defines.get("USB_VENDOR_ATMEL") != ATMEL_VID:
        raise ValueError("AVRDUDE usbdevs.h does not declare USB_VENDOR_ATMEL 0x03eb")
    if defines.get("USB_VENDOR_MICROCHIP") != MICROCHIP_VID:
        raise ValueError(
            "AVRDUDE usbdevs.h does not declare USB_VENDOR_MICROCHIP 0x04d8"
        )

    missing = sorted(
        {
            symbol
            for entry in AVRDUDE_PRODUCTS
            for symbol in (entry.symbol, entry.vendor_symbol)
            if symbol not in defines
        }
    )
    if missing:
        raise ValueError(f"AVRDUDE usbdevs.h missing expected defines: {missing}")

    out: dict[str, dict[str, str]] = {}
    for entry in AVRDUDE_PRODUCTS:
        vid = defines[entry.vendor_symbol]
        pid = defines[entry.symbol]
        out[f"{vid}:{pid}"] = {
            "vendor": _vendor_for_vid(vid),
            "product": entry.product,
        }
    return dict(sorted(out.items()))


def _collapse_board_products(names: Iterable[str]) -> str:
    unique = sorted(set(names))
    if unique == ["Arduino M0 Pro (Programming Port)", "Arduino Zero (Programming Port)"]:
        return "Arduino Zero/M0 Pro programming port"
    if unique == ["CurrentRanger", "Moteino M0"]:
        return "LowPowerLab CurrentRanger / Moteino M0"
    if len(unique) == 1:
        return unique[0]
    return " / ".join(unique)


def parse_arduino_boards(
    text: str,
    *,
    allowed_vids: set[str] = {ATMEL_VID, MICROCHIP_VID},
    allowed_names: set[str] | None = None,
) -> dict[str, dict[str, str]]:
    """Parse Arduino-style boards.txt VID/PID declarations."""
    board_names: dict[str, str] = {}
    vids: dict[tuple[str, str], str] = {}
    pids: dict[tuple[str, str], str] = {}

    for raw_line in text.splitlines():
        line = raw_line.strip()
        name_match = _BOARD_NAME_RE.match(line)
        if name_match:
            board_names[name_match.group("board")] = name_match.group("name").strip()
            continue
        vid_match = _BOARD_VID_RE.match(line)
        if vid_match:
            key = (
                vid_match.group("board"),
                vid_match.group("index_a") or vid_match.group("index_b"),
            )
            vids[key] = vid_match.group("vid").lower()
            continue
        pid_match = _BOARD_PID_RE.match(line)
        if pid_match:
            key = (
                pid_match.group("board"),
                pid_match.group("index_a") or pid_match.group("index_b"),
            )
            pids[key] = pid_match.group("pid").lower()

    names_by_vidpid: dict[str, list[str]] = {}
    for key, vid in vids.items():
        if vid not in allowed_vids or key not in pids:
            continue
        board_name = board_names.get(key[0])
        if board_name is None:
            continue
        if allowed_names is not None and board_name not in allowed_names:
            continue
        names_by_vidpid.setdefault(f"{vid}:{pids[key]}", []).append(board_name)

    return {
        vidpid: {
            "vendor": _vendor_for_vid(vidpid.split(":", 1)[0]),
            "product": _collapse_board_products(names),
        }
        for vidpid, names in sorted(names_by_vidpid.items())
    }


def _latest_lowpowerlab_samd_archive_url(package_index_text: str) -> str | None:
    data = json.loads(package_index_text)
    platforms = []
    for package in data.get("packages", []):
        if package.get("name") != "Moteino":
            continue
        for platform in package.get("platforms", []):
            name = str(platform.get("name", ""))
            if platform.get("architecture") == "samd" and "LowPowerLab SAMD Boards" in name:
                if "skip" not in name.lower():
                    platforms.append(platform)
    if not platforms:
        return None
    platforms.sort(key=lambda item: tuple(int(part) for part in item["version"].split(".")))
    return str(platforms[-1]["url"])


def parse_lowpowerlab_package(
    package_index_text: str,
    *,
    fetch_bytes: Callable[[str], bytes] = _fetch_bytes,
) -> dict[str, dict[str, str]]:
    """Fetch and parse the latest LowPowerLab SAMD boards.txt from its index."""
    archive_url = _latest_lowpowerlab_samd_archive_url(package_index_text)
    if archive_url is None:
        return {}
    archive = fetch_bytes(archive_url)
    with zipfile.ZipFile(io.BytesIO(archive)) as zf:
        boards_name = next((name for name in zf.namelist() if name.endswith("/boards.txt")), None)
        if boards_name is None:
            raise ValueError(f"LowPowerLab archive lacks boards.txt: {archive_url}")
        boards_text = zf.read(boards_name).decode("utf-8", errors="replace")
    return parse_arduino_boards(
        boards_text,
        allowed_names=LOWPOWERLAB_ALLOWED_NAMES,
    )


def collect_first_party(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    pyedbglib_url: str = PYEDBGLIB_TOOLINFO_URL,
    pykitinfo_url: str = PYKITINFO_TOOLS_URL,
) -> dict[str, dict[str, str]]:
    entries: dict[str, dict[str, str]] = {}

    try:
        toolinfo_text = fetch_text(pyedbglib_url)
    except Exception as e:
        print(f"warning: {pyedbglib_url}: fetch failed: {e}", file=sys.stderr)
    else:
        rows = parse_pyedbglib_toolinfo(toolinfo_text)
        entries.update(_to_entry_map(rows, vid=ATMEL_VID))
        print(f"pyedbglib toolinfo: {len(rows)} PID(s)", file=sys.stderr)

    try:
        tools_text = fetch_text(pykitinfo_url)
    except Exception as e:
        print(f"warning: {pykitinfo_url}: fetch failed: {e}", file=sys.stderr)
    else:
        rows = parse_pykitinfo_tools(tools_text)
        entries.update(_to_entry_map(rows, vid=MICROCHIP_VID))
        print(f"pykitinfo tools: {len(rows)} PID(s)", file=sys.stderr)

    return dict(sorted(entries.items()))


def collect_supplemental(
    *,
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_bytes: Callable[[str], bytes] = _fetch_bytes,
    avrdude_url: str = AVRDUDE_USBDEVS_URL,
    arduino_boards_urls: Iterable[str] = (
        ARDUINO_SAMD_BOARDS_URL,
        ARDUINO_MEGAAVR_BOARDS_URL,
    ),
    lowpowerlab_index_url: str = LOWPOWERLAB_PACKAGE_INDEX_URL,
) -> dict[str, dict[str, str]]:
    entries: dict[str, dict[str, str]] = {}

    try:
        avrdude_text = fetch_text(avrdude_url)
    except Exception as e:
        print(f"warning: {avrdude_url}: fetch failed: {e}", file=sys.stderr)
    else:
        rows = parse_avrdude_usbdevs(avrdude_text)
        entries.update(rows)
        print(f"avrdude usbdevs: {len(rows)} PID(s)", file=sys.stderr)

    for url in arduino_boards_urls:
        try:
            boards_text = fetch_text(url)
        except Exception as e:
            print(f"warning: {url}: fetch failed: {e}", file=sys.stderr)
            continue
        rows = parse_arduino_boards(boards_text)
        entries = _merge_fill_gaps(entries, rows)
        print(f"arduino boards: {url}: {len(rows)} PID(s)", file=sys.stderr)

    try:
        index_text = fetch_text(lowpowerlab_index_url)
    except Exception as e:
        print(f"warning: {lowpowerlab_index_url}: fetch failed: {e}", file=sys.stderr)
    else:
        rows = parse_lowpowerlab_package(index_text, fetch_bytes=fetch_bytes)
        entries = _merge_fill_gaps(entries, rows)
        print(f"lowpowerlab boards: {len(rows)} PID(s)", file=sys.stderr)

    return dict(sorted(entries.items()))


def collect(
    *,
    tier: Literal["first-party", "supplemental", "all"] = "all",
    fetch_text: Callable[[str], str] = _fetch_text,
    fetch_bytes: Callable[[str], bytes] = _fetch_bytes,
) -> dict[str, dict[str, str]]:
    if tier == "first-party":
        return collect_first_party(fetch_text=fetch_text)
    if tier == "supplemental":
        return collect_supplemental(fetch_text=fetch_text, fetch_bytes=fetch_bytes)
    first_party = collect_first_party(fetch_text=fetch_text)
    supplemental = collect_supplemental(fetch_text=fetch_text, fetch_bytes=fetch_bytes)
    return _merge_fill_gaps(first_party, supplemental)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument(
        "--tier",
        choices=("first-party", "supplemental", "all"),
        default="all",
        help="Source priority tier to emit.",
    )
    args = parser.parse_args()

    entries = collect(tier=args.tier)
    args.out.write_text(
        json.dumps(OrderedDict(sorted(entries.items())), indent=2, ensure_ascii=False)
        + "\n",
        encoding="utf-8",
    )
    print(f"wrote {args.out}: {len(entries)} Microchip/Atmel PID(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

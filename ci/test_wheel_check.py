"""Tests for ci/wheel_check.py (ported from zccache's ci/tests/test_wheel_check.py)."""

from __future__ import annotations

import io
import struct
import sys
import unittest
import zipfile
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import wheel_check  # noqa: E402

REPO = Path(__file__).resolve().parents[1]


def elf64(section_names: list[str]) -> bytes:
    """Minimal little-endian ELF64 with a section table and .shstrtab."""
    names = ["", *section_names, ".shstrtab"]
    strtab = b""
    offsets = []
    for name in names:
        offsets.append(len(strtab))
        strtab += name.encode() + b"\0"
    header_size, entry = 64, 64
    strtab_offset = header_size
    shoff = strtab_offset + len(strtab)
    header = bytearray(header_size)
    header[:4] = b"\x7fELF"
    header[4], header[5], header[6] = 2, 1, 1
    struct.pack_into("<Q", header, 0x28, shoff)
    struct.pack_into("<HHH", header, 0x3A, entry, len(names), len(names) - 1)
    table = bytearray()
    for index, offset in enumerate(offsets):
        section = bytearray(entry)
        struct.pack_into("<I", section, 0, offset)
        if index == len(names) - 1:
            struct.pack_into("<Q", section, 0x18, strtab_offset)
        table += section
    return bytes(header) + strtab + bytes(table)


def macho64(segment_names: list[str], symbol_types: list[int] | None = None) -> bytes:
    """Minimal Mach-O 64 with segment commands and an optional LC_SYMTAB."""
    commands = b""
    ncmds = len(segment_names)
    for name in segment_names:
        command = bytearray(72)
        struct.pack_into("<II", command, 0, wheel_check.LC_SEGMENT_64, 72)
        command[8 : 8 + len(name)] = name.encode()
        commands += command
    symbols = b""
    if symbol_types is not None:
        ncmds += 1
        symoff = 32 + len(commands) + 24
        symtab = bytearray(24)
        struct.pack_into("<IIII", symtab, 0, wheel_check.LC_SYMTAB, 24, symoff, len(symbol_types))
        commands += symtab
        for n_type in symbol_types:
            entry = bytearray(16)
            entry[4] = n_type
            symbols += entry
    header = bytearray(32)
    header[:4] = b"\xcf\xfa\xed\xfe"
    struct.pack_into("<II", header, 16, ncmds, len(commands))
    return bytes(header) + commands + symbols


def fat_macho(*slices: bytes) -> bytes:
    header = bytearray(8 + 20 * len(slices))
    header[:4] = wheel_check.MACHO_FAT
    struct.pack_into(">I", header, 4, len(slices))
    body = b""
    offset = len(header)
    for index, thin in enumerate(slices):
        struct.pack_into(">IIIII", header, 8 + index * 20, 0, 0, offset + len(body), len(thin), 0)
        body += thin
    return bytes(header) + body


def pe(section_names: list[str], long_names: list[str] | None = None) -> bytes:
    long_names = long_names or []
    pe_offset = 0x40
    optional_size = 0
    table_offset = pe_offset + 4 + 20 + optional_size
    strings = bytearray(4)
    raw_names = [name.encode() for name in section_names]
    for name in long_names:
        raw_names.append(f"/{len(strings)}".encode())
        strings += name.encode() + b"\0"
    symtab = table_offset + 40 * len(raw_names)
    data = bytearray(symtab)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, pe_offset)
    data[pe_offset : pe_offset + 4] = b"PE\0\0"
    coff = pe_offset + 4
    struct.pack_into("<H", data, coff + 2, len(raw_names))
    struct.pack_into("<II", data, coff + 8, symtab if long_names else 0, 0)
    struct.pack_into("<H", data, coff + 16, optional_size)
    for index, raw in enumerate(raw_names):
        data[table_offset + index * 40 : table_offset + index * 40 + len(raw)] = raw
    struct.pack_into("<I", strings, 0, len(strings))
    return bytes(data) + (bytes(strings) if long_names else b"")


def wheel(members: dict[str, bytes]) -> tuple[str, int, zipfile.ZipFile]:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, content in members.items():
            archive.writestr(name, content)
    size = buffer.tell()
    buffer.seek(0)
    return "fbuild-0.0.0-cp310-abi3-any.whl", size, zipfile.ZipFile(buffer)


class DebugSectionTests(unittest.TestCase):
    def test_elf_with_dwarf_is_rejected(self) -> None:
        kind, debug = wheel_check.debug_sections(elf64([".text", ".debug_info", ".debug_line"]))
        self.assertEqual(kind, "ELF")
        self.assertEqual(debug, [".debug_info", ".debug_line"])

    def test_stripped_elf_passes_and_keeps_gdb_scripts_exemption(self) -> None:
        self.assertEqual(
            wheel_check.debug_sections(elf64([".text", ".symtab", ".debug_gdb_scripts"])), ("ELF", [])
        )

    def test_compressed_elf_debug_is_rejected(self) -> None:
        self.assertEqual(wheel_check.debug_sections(elf64([".zdebug_info"]))[1], [".zdebug_info"])

    def test_macho_dwarf_segment_is_rejected_thin_and_fat(self) -> None:
        self.assertEqual(wheel_check.debug_sections(macho64(["__TEXT", "__DWARF"])), ("Mach-O", ["__DWARF"]))
        self.assertEqual(wheel_check.debug_sections(macho64(["__TEXT", "__LINKEDIT"])), ("Mach-O", []))
        fat = fat_macho(macho64(["__TEXT"]), macho64(["__TEXT", "__DWARF"]))
        self.assertEqual(wheel_check.debug_sections(fat), ("Mach-O", ["__DWARF"]))

    def test_macho_stabs_debug_map_is_rejected(self) -> None:
        # N_SECT|N_EXT (0x0f) is an ordinary symbol; N_OSO (0x66) is a STABS
        # debug-map entry that an unstripped split-debuginfo link leaves.
        clean = macho64(["__TEXT"], symbol_types=[0x0F, 0x0F])
        self.assertEqual(wheel_check.debug_sections(clean), ("Mach-O", []))
        dirty = macho64(["__TEXT"], symbol_types=[0x0F, 0x66])
        self.assertEqual(wheel_check.debug_sections(dirty), ("Mach-O", [wheel_check.MACHO_STABS]))

    def test_pe_debug_sections_including_long_names(self) -> None:
        self.assertEqual(wheel_check.debug_sections(pe([".text", ".rdata"])), ("PE", []))
        self.assertEqual(
            wheel_check.debug_sections(pe([".text"], long_names=[".debug_info"])), ("PE", [".debug_info"])
        )

    def test_non_binaries_are_ignored(self) -> None:
        self.assertIsNone(wheel_check.debug_sections(b"#!/usr/bin/env python\n"))


class WheelTests(unittest.TestCase):
    def test_wheel_with_unstripped_binary_fails(self) -> None:
        problems = wheel_check.check_wheels(
            [wheel({"fbuild/_native.abi3.so": elf64([".debug_info"]), "fbuild/__init__.py": b"x = 1\n"})]
        )
        self.assertEqual(len(problems), 1)
        self.assertIn("fbuild/_native.abi3.so", problems[0])
        self.assertIn(".debug_info", problems[0])

    def test_clean_wheel_passes(self) -> None:
        self.assertEqual(wheel_check.check_wheels([wheel({"fbuild/_native.abi3.so": elf64([".text"])})]), [])

    def test_wheel_and_release_budgets(self) -> None:
        candidate = wheel({"fbuild/_native.abi3.so": elf64([".text"])})
        other = wheel({"fbuild/_native.abi3.so": elf64([".text"])})
        with mock.patch.object(wheel_check, "MAX_WHEEL_BYTES", candidate[1] - 1), mock.patch.object(
            wheel_check, "MAX_RELEASE_BYTES", 2 * candidate[1] - 1
        ):
            problems = wheel_check.check_wheels([candidate, other])
        self.assertEqual(sum("budget is" in problem for problem in problems), 3)


class WorkflowWiringTests(unittest.TestCase):
    def test_release_workflow_runs_the_check_before_upload(self) -> None:
        workflow = (REPO / ".github/workflows/release-auto.yml").read_text(encoding="utf-8")
        build = workflow.index("publish.build_all_wheels(")
        check = workflow.index("python ci/wheel_check.py dist/wheels/*.whl")
        upload = workflow.index("name: pypi-fbuild")
        self.assertLess(build, check)
        self.assertLess(check, upload)

    def test_release_lanes_use_dist_profile_and_ship_sidecars(self) -> None:
        workflow = (REPO / ".github/workflows/release-auto.yml").read_text(encoding="utf-8")
        self.assertIn("cargo_profile: dist", workflow)
        self.assertIn("pattern: debug-*", workflow)
        self.assertIn('package_name="fbuild-${version}-${target}-debug"', workflow)
        template = (REPO / ".github/workflows/template_native_build.yml").read_text(encoding="utf-8")
        self.assertIn("name: Stage debug sidecars", template)
        self.assertIn("name: debug-${{ inputs.target }}", template)


if __name__ == "__main__":
    unittest.main()

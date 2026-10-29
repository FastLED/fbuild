"""Tests for ci/check_glibc_floor.py (FastLED/fbuild#1629)."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_glibc_floor import (  # noqa: E402
    Version,
    max_version,
    parse_requirements,
    violations,
)

SCRIPT = Path(__file__).resolve().parent / "check_glibc_floor.py"

SYMBOL_TABLE = """\
0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.2.5 __libc_start_main
0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.2.5 fopen64
0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.16 memfd_create
0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.17 clock_gettime
"""

TOO_NEW_TABLE = SYMBOL_TABLE + (
    "0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.27 memfd_create\n"
    "0000000000000000      DF *UND*\t0000000000000000  GLIBC_2.28 statx\n"
)


class VersionTests(unittest.TestCase):
    def test_patch_defaults_to_zero(self) -> None:
        self.assertEqual(Version(2, 17, 0), Version.parse("2.17"))

    def test_numeric_not_lexical_ordering(self) -> None:
        self.assertLess(Version.parse("2.2"), Version.parse("2.9"))
        self.assertLess(Version.parse("2.16"), Version.parse("2.17"))

    def test_rejects_malformed_floor(self) -> None:
        with self.assertRaises(ValueError):
            Version.parse("2")


class ParseTests(unittest.TestCase):
    def test_collects_symbols_per_version(self) -> None:
        requirements = parse_requirements(SYMBOL_TABLE)
        self.assertEqual(3, len(requirements))
        self.assertEqual(
            {"__libc_start_main", "fopen64"}, requirements[Version(2, 2, 5)]
        )
        self.assertEqual({"memfd_create"}, requirements[Version(2, 16, 0)])

    def test_empty_table_has_no_max(self) -> None:
        self.assertEqual({}, parse_requirements(""))
        self.assertIsNone(max_version(parse_requirements("")))


class ViolationTests(unittest.TestCase):
    def test_at_or_below_floor_is_clean(self) -> None:
        binary = Path("fbuild")
        requirements = parse_requirements(SYMBOL_TABLE)
        self.assertEqual([], violations(binary, requirements, Version(2, 17, 0)))
        self.assertEqual([], violations(binary, requirements, Version(2, 40, 0)))

    def test_above_floor_reports_symbols(self) -> None:
        findings = violations(
            Path("fbuild"), parse_requirements(TOO_NEW_TABLE), Version(2, 17, 0)
        )
        self.assertEqual(
            [Version(2, 27, 0), Version(2, 28, 0)], [f.version for f in findings]
        )
        self.assertEqual(("memfd_create",), findings[0].symbols)
        self.assertEqual(("statx",), findings[1].symbols)


class CommandLineTests(unittest.TestCase):
    def run_guard(
        self, directory: Path, *args: str
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            capture_output=True,
            text=True,
            check=False,
            cwd=directory,
        )

    def fake_objdump(self, directory: Path, table: str) -> Path:
        table_path = directory / "table.txt"
        table_path.write_text(table, encoding="utf-8")
        tool = directory / "objdump"
        tool.write_text(f'#!/bin/sh\ncat "{table_path}"\n', encoding="utf-8")
        tool.chmod(0o755)
        return tool

    def test_passes_on_green_binary(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            tool = self.fake_objdump(directory, SYMBOL_TABLE)
            (directory / "fbuild").write_text("", encoding="utf-8")
            result = self.run_guard(
                directory, "--objdump", str(tool), "--floor", "2.17", "fbuild"
            )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("max GLIBC_2.17", result.stdout)

    def test_fails_on_too_new_binary(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            tool = self.fake_objdump(directory, TOO_NEW_TABLE)
            (directory / "fbuild").write_text("", encoding="utf-8")
            result = self.run_guard(
                directory, "--objdump", str(tool), "--floor", "2.17", "fbuild"
            )
        self.assertEqual(1, result.returncode, result.stderr)
        self.assertIn("requires GLIBC_2.28 (floor 2.17)", result.stderr)
        self.assertIn("statx", result.stderr)

    def test_missing_binary_is_inspection_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            tool = self.fake_objdump(directory, SYMBOL_TABLE)
            result = self.run_guard(directory, "--objdump", str(tool), "absent")
        self.assertEqual(2, result.returncode, result.stderr)
        self.assertIn("no such file", result.stderr)


if __name__ == "__main__":
    unittest.main()

"""Tests for ci/check_flash_offsets.py -- the ESP32 bootloader-offset gate.

A wrong bootloader offset is a bricking bug (#278 shipped `0x0` where the ROM
loads at `0x2000`), so this gate must never report a chip as verified when it
does not have an authoritative value. Its documented promise is "never silently
passes"; the load-bearing cases below are the two ways it used to break that
promise, both from a hard-coded `0x1000` fallback standing in for an
authoritative `platform.txt` default:

  * a boards.txt with no sibling platform.txt at all, and
  * a platform.txt that does not declare `build.bootloader_addr`.

Both produced "All N config(s) match" with exit code 0. They are hard failures
now, while an explicit `<chip>.build.bootloader_addr` and a real platform.txt
default both still verify normally. The end-to-end cases drive the real script
as a subprocess in a repo-shaped temp tree, so `configs_dir()`'s relative
discovery is exercised too.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from ci import check_flash_offsets as flash

SCRIPT = Path(flash.__file__).resolve()
REAL_CONFIGS = (
    SCRIPT.parent.parent / "crates" / "fbuild-build-esp" / "src" / "esp32" / "configs"
)


def boards_text(spec: dict[str, str | None]) -> str:
    """A boards.txt fragment: every chip declares build.mcu; addr is optional."""
    lines: list[str] = []
    for chip, addr in spec.items():
        lines.append(f"{chip}.name={chip.upper()} board")
        lines.append(f"{chip}.build.mcu={chip}")
        if addr is not None:
            lines.append(f"{chip}.build.bootloader_addr={addr}")
    return "\n".join(lines) + "\n"


DEFAULT_SPEC: dict[str, str | None] = {
    "esp32": None,  # relies on the platform.txt default (0x1000)
    "esp32s2": None,  # also relies on the default
    "esp32s3": "0x0",
    "esp32c5": "0x2000",
}


def write_config(
    directory: Path,
    mcu: str,
    boot: str | None,
    part: str = "0x8000",
    firmware: str = "0x10000",
) -> Path:
    offsets: dict[str, str] = {"partitions": part, "firmware": firmware}
    if boot is not None:
        offsets["bootloader"] = boot
    path = directory / f"{mcu}.json"
    path.write_text(
        json.dumps({"mcu": mcu, "esptool": {"flash_offsets": offsets}}),
        encoding="utf-8",
    )
    return path


def write_default_fixture(
    directory: Path, boot_overrides: dict[str, str | None] | None = None
) -> None:
    """One config per DEFAULT_SPEC chip, matching the offsets the gate expects."""
    overrides = boot_overrides or {}
    for chip, addr in DEFAULT_SPEC.items():
        write_config(
            directory, chip, overrides.get(chip, addr if addr is not None else "0x1000")
        )


def source(
    platform_text: str | None, spec: dict[str, str | None] | None = None
) -> flash.Source:
    return flash.Source(
        boards_text(spec or DEFAULT_SPEC),
        platform_text,
        "test fixture",
    )


class NormalizeOffsetTests(unittest.TestCase):
    def test_leading_zeros_and_case_fold(self) -> None:
        self.assertEqual(flash.normalize_offset("0X00001000"), "0x1000")
        self.assertEqual(flash.normalize_offset(" 0x0 "), "0x0")

    def test_does_not_invent_a_prefix(self) -> None:
        self.assertEqual(flash.normalize_offset("1000"), "1000")


class ParsePlatformDefaultTests(unittest.TestCase):
    def test_reads_the_declared_default(self) -> None:
        self.assertEqual(
            flash.parse_platform_default("a=1\nbuild.bootloader_addr=0x2000\n"),
            "0x2000",
        )

    def test_absent_key_is_unresolved_not_a_constant(self) -> None:
        self.assertIsNone(flash.parse_platform_default("a=1\n"))
        self.assertIsNone(flash.parse_platform_default(""))
        self.assertIsNone(flash.parse_platform_default("build.bootloader_addr=\n"))

    def test_missing_platform_txt_is_unresolved(self) -> None:
        self.assertIsNone(flash.parse_platform_default(None))


class ParseBoardsTests(unittest.TestCase):
    def test_explicit_chip_overrides_are_captured(self) -> None:
        addrs = flash.parse_boards_bootloader_addr(
            boards_text({"esp32c5": "0x2000", "esp32": None})
        )
        self.assertEqual(addrs, {"esp32c5": "0x2000"})

    def test_menu_scoped_keys_are_ignored(self) -> None:
        text = "esp32c5.build.bootloader_addr=0x2000\nesp32c5.menu.PartitionScheme.x.build.bootloader_addr=0x0\n"
        self.assertEqual(
            flash.parse_boards_bootloader_addr(text), {"esp32c5": "0x2000"}
        )
        self.assertEqual(flash.parse_known_chips(text), set())

    def test_known_chips_are_distinct_mcu_values(self) -> None:
        text = boards_text({"esp32c6": "0x0"}) + "esp32c6.build.mcu=esp32c6\n"
        self.assertEqual(flash.parse_known_chips(text), {"esp32c6"})


class AuthoritativeOffsetTests(unittest.TestCase):
    def test_explicit_entry_wins_over_the_default(self) -> None:
        resolved = flash.authoritative_offset(
            "esp32c5", {"esp32c5": "0x2000"}, {"esp32c5"}, "0x1000"
        )
        self.assertEqual(resolved, "0x2000")

    def test_known_chip_falls_back_to_the_platform_default(self) -> None:
        self.assertEqual(
            flash.authoritative_offset("esp32", {}, {"esp32"}, "0x1000"), "0x1000"
        )

    def test_unknown_chip_is_unresolved(self) -> None:
        self.assertIsNone(
            flash.authoritative_offset("esp32c9", {}, {"esp32"}, "0x1000")
        )

    def test_known_chip_without_a_default_is_unresolved(self) -> None:
        self.assertIsNone(flash.authoritative_offset("esp32", {}, {"esp32"}, None))


class CheckConfigTests(unittest.TestCase):
    def test_matching_offsets_pass(self) -> None:
        result = flash.check_config(
            "esp32c5",
            {"bootloader": "0x2000", "partitions": "0x8000", "firmware": "0x10000"},
            source("build.bootloader_addr=0x1000\n"),
            {"esp32c5": "0x2000"},
            set(DEFAULT_SPEC),
            "0x1000",
        )
        self.assertEqual(result.failures, [])
        self.assertTrue(result.row.endswith("PASS"))

    def test_wrong_bootloader_names_both_values(self) -> None:
        result = flash.check_config(
            "esp32c5",
            {"bootloader": "0x0", "partitions": "0x8000", "firmware": "0x10000"},
            source("build.bootloader_addr=0x1000\n"),
            {"esp32c5": "0x2000"},
            set(DEFAULT_SPEC),
            "0x1000",
        )
        self.assertEqual(len(result.failures), 1)
        self.assertIn(
            "bootloader offset '0x0' != authoritative '0x2000'", result.failures[0]
        )

    def test_missing_bootloader_key_fails(self) -> None:
        result = flash.check_config(
            "esp32c5",
            {"partitions": "0x8000", "firmware": "0x10000"},
            source("build.bootloader_addr=0x1000\n"),
            {"esp32c5": "0x2000"},
            set(DEFAULT_SPEC),
            "0x1000",
        )
        self.assertIn("no esptool.flash_offsets.bootloader", result.failures[0])

    def test_partition_and_firmware_drift_fail(self) -> None:
        result = flash.check_config(
            "esp32c5",
            {"bootloader": "0x2000", "partitions": "0x9000", "firmware": "0x20000"},
            source("build.bootloader_addr=0x1000\n"),
            {"esp32c5": "0x2000"},
            set(DEFAULT_SPEC),
            "0x1000",
        )
        self.assertEqual(len(result.failures), 2)
        self.assertIn("partitions offset", result.failures[0])
        self.assertIn("firmware offset", result.failures[1])

    def test_unknown_chip_reports_a_coverage_gap(self) -> None:
        result = flash.check_config(
            "esp32c9",
            {"bootloader": "0x0", "partitions": "0x8000", "firmware": "0x10000"},
            source("build.bootloader_addr=0x1000\n"),
            {},
            {"esp32"},
            "0x1000",
        )
        self.assertIn(
            "chip is unknown to the authoritative boards.txt", result.failures[0]
        )


class FailOpenRegressionTests(unittest.TestCase):
    """The two paths that used to fabricate `0x1000` and report a pass."""

    def test_default_reliant_chip_without_platform_txt_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            write_default_fixture(directory)
            report = flash.evaluate(directory, source(None))
        self.assertEqual(report.configs, 4)
        self.assertEqual(len(report.failures), 2, report.failures)
        self.assertTrue(
            all("refusing to guess" in f for f in report.failures), report.failures
        )
        self.assertIn("no platform.txt accompanies this boards.txt", report.failures[0])

    def test_platform_txt_without_the_key_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            write_default_fixture(directory)
            report = flash.evaluate(directory, source("version=3.3.12\n"))
        self.assertEqual(len(report.failures), 2, report.failures)
        self.assertIn(
            "platform.txt does not declare build.bootloader_addr", report.failures[0]
        )

    def test_a_declared_default_still_verifies(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            write_default_fixture(directory)
            report = flash.evaluate(directory, source("build.bootloader_addr=0x1000\n"))
        self.assertEqual(report.failures, [])
        self.assertEqual(report.configs, 4)

    def test_a_changed_upstream_default_fails_every_reliant_chip(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            write_default_fixture(directory)
            report = flash.evaluate(directory, source("build.bootloader_addr=0x2000\n"))
        self.assertEqual(len(report.failures), 2, report.failures)
        self.assertIn("!= authoritative '0x2000'", report.failures[0])


class RealRepoConfigTests(unittest.TestCase):
    def test_every_real_config_pins_partitions_and_firmware(self) -> None:
        configs = sorted(REAL_CONFIGS.glob("esp32*.json"))
        self.assertTrue(configs, f"no esp32 configs under {REAL_CONFIGS}")
        for path in configs:
            mcu, offsets = flash.load_config_offsets(path)
            self.assertEqual(
                flash.normalize_offset(offsets.get("partitions", "")), "0x8000", mcu
            )
            self.assertEqual(
                flash.normalize_offset(offsets.get("firmware", "")), "0x10000", mcu
            )

    def test_real_configs_pass_when_boards_txt_quotes_their_own_offsets(self) -> None:
        spec: dict[str, str | None] = {
            flash.load_config_offsets(path)[0]: flash.load_config_offsets(path)[1].get(
                "bootloader"
            )
            for path in sorted(REAL_CONFIGS.glob("esp32*.json"))
        }
        with tempfile.TemporaryDirectory() as tmp:
            report = flash.evaluate(REAL_CONFIGS, source(None, spec))
        self.assertEqual(report.failures, [])
        self.assertEqual(report.configs, len(spec))


class EndToEndTests(unittest.TestCase):
    """Drive the real script in a repo-shaped tree so configs_dir() is exercised."""

    def run_script(
        self, root: Path, *arguments: str
    ) -> subprocess.CompletedProcess[str]:
        env = {k: v for k, v in os.environ.items() if k != "FBUILD_ESP32_BOARDS_TXT"}
        return subprocess.run(
            [sys.executable, str(root / "ci" / "check_flash_offsets.py"), *arguments],
            capture_output=True,
            text=True,
            env=env,
            timeout=120,
        )

    def fixture(self, root: Path, platform_text: str | None) -> Path:
        (root / "ci").mkdir(parents=True)
        (root / "ci" / "check_flash_offsets.py").write_text(
            SCRIPT.read_text(encoding="utf-8"), encoding="utf-8"
        )
        configs = root / "crates" / "fbuild-build-esp" / "src" / "esp32" / "configs"
        configs.mkdir(parents=True)
        write_default_fixture(configs)
        boards = root / "boards.txt"
        boards.write_text(boards_text(DEFAULT_SPEC), encoding="utf-8")
        if platform_text is not None:
            (root / "platform.txt").write_text(platform_text, encoding="utf-8")
        return boards

    def test_matching_source_exits_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            boards = self.fixture(root, "build.bootloader_addr=0x1000\n")
            done = self.run_script(root, "--boards-txt", str(boards))
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn(
            "platform.txt default build.bootloader_addr = 0x1000", done.stdout
        )
        self.assertIn("All 4 esp32 config(s) match", done.stdout)

    def test_missing_platform_txt_exits_nonzero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            boards = self.fixture(root, None)
            done = self.run_script(root, "--boards-txt", str(boards))
        self.assertEqual(done.returncode, 1, done.stdout)
        self.assertIn("refusing to guess", done.stdout)
        self.assertIn("FAILED: 2 flash-offset problem(s)", done.stdout)

    def test_stale_config_exits_nonzero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            boards = self.fixture(root, "build.bootloader_addr=0x1000\n")
            write_config(
                root / "crates" / "fbuild-build-esp" / "src" / "esp32" / "configs",
                "esp32c5",
                "0x0",
            )
            done = self.run_script(root, "--boards-txt", str(boards))
        self.assertEqual(done.returncode, 1, done.stdout)
        self.assertIn("!= authoritative '0x2000'", done.stdout)

    def test_boards_txt_without_a_value_is_a_usage_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root, "build.bootloader_addr=0x1000\n")
            done = self.run_script(root, "--boards-txt")
        self.assertEqual(done.returncode, 1)
        self.assertIn("--boards-txt needs a path", done.stderr)

    def test_help_exits_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root, "build.bootloader_addr=0x1000\n")
            done = self.run_script(root, "--help")
        self.assertEqual(done.returncode, 0)
        self.assertIn("Usage:", done.stdout)


if __name__ == "__main__":
    unittest.main()

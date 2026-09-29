"""Tests for path-based board selection (ci/select_boards.py).

A push to main dispatches only the per-board workflows whose trigger paths
the push touched; the nightly sweep dispatches every board so README badges
refresh on a schedule.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from select_boards import matrix_entries, path_matches, select_workflows  # noqa: E402

FAMILIES = {
    "avr": {"crate_paths": ["crates/fbuild-build-mcu/src/avr/**"]},
    "esp32": {"crate_paths": ["crates/fbuild-build-esp/src/esp32/**"]},
}
COMMON = ["crates/fbuild-core/**", "Cargo.lock", "crates/fbuild-build-mcu/*argo.toml"]
BOARDS = [
    {"workflow": "build-uno.yml", "test_dir": "tests/platform/uno", "family": "avr", "core": True},
    {"workflow": "build-attiny85.yml", "test_dir": "tests/platform/attiny85", "family": "avr"},
    {"workflow": "build-esp32c3.yml", "test_dir": "tests/platform/esp32c3", "family": "esp32"},
]


def select(changed):
    return select_workflows(BOARDS, FAMILIES, COMMON, changed)


class PathMatchTests(unittest.TestCase):
    def test_double_star_crosses_directories(self):
        self.assertTrue(path_matches("crates/fbuild-core/**", "crates/fbuild-core/src/a/b.rs"))

    def test_single_star_stays_in_one_segment(self):
        self.assertTrue(path_matches("crates/fbuild-build-mcu/*argo.toml", "crates/fbuild-build-mcu/Cargo.toml"))
        self.assertFalse(path_matches("crates/x/*.rs", "crates/x/sub/a.rs"))

    def test_literal(self):
        self.assertTrue(path_matches("Cargo.lock", "Cargo.lock"))
        self.assertFalse(path_matches("Cargo.lock", "sub/Cargo.lock"))


class SelectTests(unittest.TestCase):
    def test_docs_only_selects_nothing(self):
        self.assertEqual([], select(["README.md", "docs/x.md"]))

    def test_board_test_dir_selects_only_that_board(self):
        self.assertEqual(["build-attiny85.yml"], select(["tests/platform/attiny85/platformio.ini"]))

    def test_family_code_selects_the_family(self):
        self.assertEqual(
            ["build-attiny85.yml", "build-uno.yml"],
            select(["crates/fbuild-build-mcu/src/avr/mod.rs"]),
        )

    def test_shared_code_selects_core_boards_only(self):
        self.assertEqual(["build-uno.yml"], select(["crates/fbuild-core/src/lib.rs"]))

    def test_own_workflow_file_selects_board(self):
        self.assertEqual(["build-esp32c3.yml"], select([".github/workflows/build-esp32c3.yml"]))

    def test_unknown_diff_selects_everything(self):
        self.assertEqual(sorted(b["workflow"] for b in BOARDS), select(None))


class MatrixTests(unittest.TestCase):
    """PR runs call template_build.yml directly, so they need board records."""

    boards = [
        {**b, "workflow_name": b["workflow"], "env_name": "e", "firmware_ext": "hex"}
        for b in BOARDS
    ]
    families = {**FAMILIES, "esp32": {**FAMILIES["esp32"], "toolchain_cache": True}}

    def test_esp_change_yields_only_esp_records(self):
        entries = matrix_entries(self.boards, self.families, ["build-esp32c3.yml"])
        self.assertEqual(["build-esp32c3.yml"], [e["workflow"] for e in entries])
        self.assertEqual("tests/platform/esp32c3", entries[0]["test_dir"])
        self.assertTrue(entries[0]["toolchain_cache"])

    def test_nothing_selected_is_empty(self):
        self.assertEqual([], matrix_entries(self.boards, self.families, []))


class RealSotTests(unittest.TestCase):
    """The checked-in SOT, not the fixture above (FastLED/fbuild#1588)."""

    def select(self, changed):
        from render_workflows import load_common_paths, load_sot

        sot = load_sot()
        return select_workflows(sot["boards"], sot["families"], load_common_paths(), changed)

    def test_include_farm_change_builds_every_esp32_variant(self):
        selected = self.select(["crates/fbuild-build-engine/src/include_farm.rs"])
        for variant in ("esp32dev", "esp32s2", "esp32s3", "esp32c3", "esp32c6", "esp32h2", "esp32p4"):
            self.assertIn(f"build-{variant}.yml", selected)

    def test_other_engine_change_builds_core_boards_only(self):
        selected = self.select(["crates/fbuild-build-engine/src/compiler.rs"])
        self.assertNotIn("build-esp32p4.yml", selected)
        self.assertIn("build-esp32dev.yml", selected)


if __name__ == "__main__":
    unittest.main()

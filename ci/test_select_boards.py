"""Tests for path-based board selection (ci/select_boards.py).

A push to main dispatches only the per-board workflows whose trigger paths
the push touched; the nightly sweep dispatches every board so README badges
refresh on a schedule.
"""
from __future__ import annotations

import io
import json
import subprocess
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

import render_workflows  # noqa: E402
import select_boards  # noqa: E402
from select_boards import (  # noqa: E402
    ZERO_SHA,
    changed_files,
    main,
    matrix_entries,
    path_matches,
    select_workflows,
)

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



class PackageCacheMatrixTests(unittest.TestCase):
    """The PR board matrix must carry a board's vendor package-cache opt-in.

    `pr_boards` calls `template_build.yml` with an explicit `with:` list taken
    from this matrix, so a cache configured only on build-<board>.yml is
    invisible to every pull request and merge-queue entry.
    """

    def test_matrix_carries_the_sot_package_cache(self):
        sot = render_workflows.load_sot()
        caches = {
            board["workflow"]: board["package_cache"]
            for board in sot["boards"]
            if board.get("package_cache")
        }
        self.assertTrue(caches, "no board opts into the package cache")
        for workflow, cache in caches.items():
            with self.subTest(workflow=workflow):
                rows = select_boards.matrix_entries(sot["boards"], sot["families"], [workflow])
                self.assertEqual(1, len(rows))
                self.assertEqual(cache["path"], rows[0]["package_cache_path"])
                self.assertEqual(cache["hash_files"], rows[0]["package_cache_hash_files"])

    def test_matrix_carries_an_empty_pair_for_every_other_board(self):
        sot = render_workflows.load_sot()
        plain = next(board for board in sot["boards"] if not board.get("package_cache"))
        rows = select_boards.matrix_entries(sot["boards"], sot["families"], [plain["workflow"]])
        self.assertEqual("", rows[0]["package_cache_path"])
        self.assertEqual("", rows[0]["package_cache_hash_files"])

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

    def test_test_only_changes_select_nothing(self):
        # FastLED/fbuild#1577: moving or editing tests must not fan out to
        # platform builds; the Linux test job already runs them.
        self.assertEqual(
            [],
            select([
                "crates/fbuild-core/tests/it/main.rs",
                "crates/fbuild-build/tests/env_isolated/avr_build.rs",
                "crates/fbuild-build-mcu/src/avr/orchestrator_tests.rs",
                "ci/test_select_boards.py",
            ]),
        )

    def test_test_change_alongside_source_still_selects(self):
        self.assertEqual(
            ["build-attiny85.yml", "build-uno.yml"],
            select(["crates/fbuild-core/tests/it/main.rs", "crates/fbuild-build-mcu/src/avr/mod.rs"]),
        )

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


class ChangedFilesTests(unittest.TestCase):
    """`changed_files` is the fail-open boundary of the whole dispatcher.

    An unknown diff must select every board; returning an empty list instead
    would silently stop platform coverage after a force-push or a rewound base.
    """

    def test_missing_or_zero_base_is_unknown_without_running_git(self) -> None:
        with patch.object(select_boards.subprocess, "run") as run:
            self.assertIsNone(changed_files("", "HEAD"))
            self.assertIsNone(changed_files(ZERO_SHA, "HEAD"))
        run.assert_not_called()

    def test_failed_git_diff_is_unknown_and_says_why(self) -> None:
        failed = subprocess.CompletedProcess([], 128, stdout="", stderr="fatal: bad revision 'nosuch'")
        with (
            patch.object(select_boards.subprocess, "run", return_value=failed),
            redirect_stderr(io.StringIO()) as err,
        ):
            self.assertIsNone(changed_files("nosuch", "HEAD"))
        message = err.getvalue()
        self.assertIn("selecting all boards", message)
        self.assertIn("fatal: bad revision", message)

    def test_successful_diff_drops_blank_lines(self) -> None:
        ok = subprocess.CompletedProcess([], 0, stdout="a.rs\nsrc/b.rs\n\n", stderr="")
        with patch.object(select_boards.subprocess, "run", return_value=ok) as run:
            self.assertEqual(["a.rs", "src/b.rs"], changed_files("base1", "head1"))
        self.assertEqual(["git", "diff", "--name-only", "base1", "head1"], run.call_args[0][0])


class MainTests(unittest.TestCase):
    """`main()` is what the workflows execute; its JSON and exit code are the contract."""

    def run_main(self, *argv, sot=None, changed=None, diff_calls=None):
        sot = sot if sot is not None else {"boards": BOARDS, "families": FAMILIES}

        def diff(base, head):
            if diff_calls is not None:
                diff_calls.append((base, head))
            return changed

        out = io.StringIO()
        with (
            patch.object(sys, "argv", ["select_boards.py", *argv]),
            patch.object(select_boards, "load_sot", return_value=sot),
            patch.object(select_boards, "load_common_paths", return_value=COMMON),
            patch.object(select_boards, "changed_files", side_effect=diff),
            redirect_stdout(out),
        ):
            rc = main()
        return rc, out.getvalue()

    def test_unknown_diff_prints_every_board_and_exits_zero(self) -> None:
        rc, out = self.run_main("--base", "", changed=None)
        self.assertEqual(0, rc)
        self.assertEqual(sorted(b["workflow"] for b in BOARDS), json.loads(out))

    def test_all_skips_the_diff_entirely(self) -> None:
        calls: list[tuple[str, str]] = []
        rc, out = self.run_main("--all", changed=["tests/platform/attiny85/platformio.ini"], diff_calls=calls)
        self.assertEqual(0, rc)
        self.assertEqual([], calls)
        # A consulted diff would have narrowed this to build-attiny85.yml.
        self.assertEqual(sorted(b["workflow"] for b in BOARDS), json.loads(out))

    def test_base_and_head_are_forwarded_to_the_diff(self) -> None:
        calls: list[tuple[str, str]] = []
        rc, out = self.run_main("--base", "abc123", "--head", "def456", changed=[], diff_calls=calls)
        self.assertEqual(0, rc)
        self.assertEqual([("abc123", "def456")], calls)
        self.assertEqual([], json.loads(out))

    def test_matrix_flag_emits_template_records(self) -> None:
        boards = [
            {**b, "workflow_name": b["workflow"], "env_name": "e", "firmware_ext": "hex"} for b in BOARDS
        ]
        sot = {
            "boards": boards,
            "families": {**FAMILIES, "esp32": {**FAMILIES["esp32"], "toolchain_cache": True}},
        }
        rc, out = self.run_main(
            "--matrix",
            "--base",
            "base1",
            changed=["tests/platform/esp32c3/platformio.ini"],
            sot=sot,
        )
        self.assertEqual(0, rc)
        entries = json.loads(out)
        self.assertEqual(["build-esp32c3.yml"], [e["workflow"] for e in entries])
        self.assertTrue(entries[0]["toolchain_cache"])
        self.assertEqual("tests/platform/esp32c3", entries[0]["test_dir"])


if __name__ == "__main__":
    unittest.main()

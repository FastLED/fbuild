"""Tests for ci/validate_boards.py -- the board-asset drift gate.

`validate-boards.yml` installs seven pinned PlatformIO platforms and runs this
script; a green job is the only thing standing between a hand-edited board JSON
and the shipped assets. Before this module the script had no test at all, and
three of its paths could not fail:

  * a board on an *installed* platform whose upstream JSON is missing was
    counted as "skipped (platform not installed)" -- 61 boards are in that
    state, including every esp32-c5/c6/p4 asset, so the job printed
    "All checked boards match PlatformIO definitions" while never looking at
    them (they are now an explicit `known-unmatched` bucket pinned in
    `ci/validate_boards_baseline.json`; a NEW one fails the gate);
  * a run that compared zero boards -- no platform installed, or none of the
    installed platforms carrying our boards -- exited 0 with that same
    "All checked boards match" line;
  * `if expected_build and expected_build != actual_build` short-circuited the
    comparison whenever PlatformIO declared nothing comparable, so an asset
    carrying build/upload fields upstream never declares passed silently.

The end-to-end cases drive `main()` against a synthetic asset tree +
`.platformio/platforms` layout, so the real counting and exit-code logic runs;
the last cases reconcile the shipped baseline against the repo it describes.
"""

from __future__ import annotations

import io
import json
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

from ci import validate_boards as vb

REPO_ROOT = Path(vb.__file__).resolve().parent.parent
ASSETS = REPO_ROOT / "crates" / "fbuild-config" / "assets" / "boards" / "json"
WORKFLOW = REPO_ROOT / ".github" / "workflows" / "validate-boards.yml"


# --- fixtures ---------------------------------------------------------------


def write_json(path: Path, doc: dict) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(doc), encoding="utf-8")
    return path


def write_board(boards_dir: Path, board_id: str, platform: str | None, **sections: object) -> Path:
    doc: dict[str, object] = {"id": board_id}
    if platform is not None:
        doc["platform"] = platform
    doc.update(sections)
    return write_json(boards_dir / f"{board_id}.json", doc)


def write_pio_board(pio_dir: Path, platform: str, board_id: str, **sections: object) -> Path:
    doc: dict[str, object] = {"id": board_id}
    doc.update(sections)
    return write_json(pio_dir / platform / "boards" / f"{board_id}.json", doc)


PIO_BUILD = {"core": "esp32", "variant": "esp32", "f_cpu": "240000000L", "extra_flags": ["-DFOO", "-DBAR"]}
OUR_BUILD = {"core": "esp32", "variant": "esp32", "f_cpu": "240000000L", "extra_flags": "-DFOO -DBAR"}


class Tree:
    """A synthetic assets/ + platforms/ pair shaped like CI's checkout."""

    def __init__(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = self._tmp.name
        root = Path(self.root)
        self.boards = root / "assets" / "json"
        self.pio = root / "home" / ".platformio" / "platforms"
        self.boards.mkdir(parents=True)
        self.pio.mkdir(parents=True)
        self.playform = root / "baseline.json"

    def close(self) -> None:
        self._tmp.cleanup()

    def run(self, *argv: str) -> tuple[int, str, str]:
        """Run main() in-process against this tree; return (rc, stdout, stderr)."""
        out, err = io.StringIO(), io.StringIO()
        with (
            mock.patch.object(vb, "assets_boards_dir", lambda: self.boards),
            mock.patch.object(vb, "pio_platforms_dir", lambda: self.pio),
            mock.patch.object(vb, "BASELINE_PATH", self.playform),
            mock.patch.object(vb.sys, "argv", ["validate_boards.py", *argv]),
            redirect_stdout(out),
            redirect_stderr(err),
        ):
            rc = vb.main()
        return rc, out.getvalue(), err.getvalue()


# --- field extraction -------------------------------------------------------


class ExtractTests(unittest.TestCase):
    def test_extract_build_keeps_only_comparable_fields(self) -> None:
        extracted = vb.extract_build({**PIO_BUILD, "unknown_key": "drop-me"})
        self.assertEqual(
            extracted,
            {"core": "esp32", "variant": "esp32", "f_cpu": "240000000L", "extra_flags": "-DFOO -DBAR"},
        )

    def test_extract_build_normalizes_list_extra_flags(self) -> None:
        self.assertEqual(vb.extract_build({"extra_flags": ["-DA", "-DB"]})["extra_flags"], "-DA -DB")

    def test_extract_build_keeps_arduino_subfields(self) -> None:
        extracted = vb.extract_build({"arduino": {"ldscript": "x.ld", "partitions": "p.csv", "vendor": "no"}})
        self.assertEqual(extracted["arduino"], {"ldscript": "x.ld", "partitions": "p.csv"})

    def test_extract_build_injects_megatinycore_defines_once(self) -> None:
        first = vb.extract_build({"core": "megatinycore", "extra_flags": "-DFOO"})
        self.assertEqual(first["extra_flags"], " ".join(["-DFOO", *vb.MEGATINYCORE_EXTRA_FLAGS]))
        # A board that already carries one of the framework defines must not
        # get a duplicate copy spliced in.
        preloaded = vb.extract_build({"core": "megatinycore", "extra_flags": vb.MEGATINYCORE_EXTRA_FLAGS[0]})
        self.assertEqual(preloaded["extra_flags"].split().count(vb.MEGATINYCORE_EXTRA_FLAGS[0]), 1)

    def test_extract_upload_keeps_only_upload_fields(self) -> None:
        extracted = vb.extract_upload({"protocol": "esptool", "speed": 460800, "not_ours": 1})
        self.assertEqual(extracted, {"protocol": "esptool", "speed": 460800})

    def test_normalize_extra_flags_handles_non_string(self) -> None:
        self.assertEqual(vb.normalize_extra_flags(None), "")
        self.assertEqual(vb.normalize_extra_flags(12), "")

    def test_diff_dicts_reports_every_direction(self) -> None:
        diffs = vb.diff_dicts(
            {"same": 1, "gone": 2, "changed": 3, "nested": {"inner": 4}},
            {"same": 1, "added": 5, "changed": 9, "nested": {"inner": 8}},
        )
        joined = "\n".join(diffs)
        self.assertIn("+ added: 5 (extra in our asset)", joined)
        self.assertIn("- gone: 2 (missing from our asset)", joined)
        self.assertIn("~ changed: expected 3, got 9", joined)
        self.assertIn("~ nested.inner: expected 4, got 8", joined)


# --- upstream lookup --------------------------------------------------------


class FindPioBoardTests(unittest.TestCase):
    def test_finds_base_platform_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            pio = Path(tmp)
            write_pio_board(pio, "espressif32", "esp32dev", build=PIO_BUILD)
            self.assertIsNotNone(vb.find_pio_board("esp32dev", "espressif32", pio))

    def test_finds_versioned_platform_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            pio = Path(tmp)
            write_pio_board(pio, "espressif32@src-abc123", "esp32dev", build=PIO_BUILD)
            self.assertIsNotNone(vb.find_pio_board("esp32dev", "espressif32", pio))

    def test_missing_board_returns_none(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            pio = Path(tmp)
            write_pio_board(pio, "espressif32", "other", build=PIO_BUILD)
            self.assertIsNone(vb.find_pio_board("esp32dev", "espressif32", pio))
            self.assertIsNone(vb.find_pio_board("esp32dev", "nope", Path(tmp) / "absent"))


# --- per-board validation ---------------------------------------------------


class ValidateBoardTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tree = Tree()
        self.addCleanup(self.tree.close)

    def _board(self, **our: object) -> Path:
        write_pio_board(self.tree.pio, "espressif32", "esp32dev", build=PIO_BUILD, upload={"protocol": "esptool"})
        return write_board(self.tree.boards, "esp32dev", "espressif32", build=OUR_BUILD, upload={"protocol": "esptool"}, **our)

    def test_matching_board(self) -> None:
        self.assertEqual(vb.validate_board(self._board(), self.tree.pio).status, vb.MATCH)

    def test_drifting_board_reports_the_field(self) -> None:
        board = write_board(
            self.tree.boards, "esp32dev", "espressif32", build={**OUR_BUILD, "f_cpu": "80000000L"}, upload={"protocol": "esptool"}
        )
        write_pio_board(self.tree.pio, "espressif32", "esp32dev", build=PIO_BUILD, upload={"protocol": "esptool"})
        outcome = vb.validate_board(board, self.tree.pio)
        self.assertEqual(outcome.status, vb.DRIFT)
        self.assertIn("build.f_cpu", "\n".join(outcome.diffs))

    def test_fbuild_extension_field_is_not_drift(self) -> None:
        board = write_board(
            self.tree.boards,
            "esp32dev",
            "espressif32",
            build={**OUR_BUILD, "cmsis_dsp_lib": "arm_cortexM4lf_math"},
            upload={"protocol": "esptool"},
        )
        write_pio_board(self.tree.pio, "espressif32", "esp32dev", build=PIO_BUILD, upload={"protocol": "esptool"})
        self.assertEqual(vb.validate_board(board, self.tree.pio).status, vb.MATCH)

    def test_upstream_without_a_build_section_still_reports_our_fields(self) -> None:
        # RED before the fix: `if expected_build and ...` made an upstream board
        # with no comparable build fields a silent match, so our asset could
        # carry anything there.
        board = self._board()
        write_pio_board(self.tree.pio, "espressif32", "esp32dev", upload={"protocol": "esptool"})
        outcome = vb.validate_board(board, self.tree.pio)
        self.assertEqual(outcome.status, vb.DRIFT)
        self.assertIn("+ build.core", "\n".join(outcome.diffs))

    def test_upstream_with_empty_upload_section_reports_our_fields(self) -> None:
        board = self._board()
        write_pio_board(self.tree.pio, "espressif32", "esp32dev", build=PIO_BUILD, upload={})
        outcome = vb.validate_board(board, self.tree.pio)
        self.assertEqual(outcome.status, vb.DRIFT)
        self.assertIn("+ upload.protocol", "\n".join(outcome.diffs))

    def test_board_without_upstream_json_is_unmatched_not_matched(self) -> None:
        board = write_board(self.tree.boards, "esp32-c5-devkitc-1", "espressif32", build=OUR_BUILD)
        (self.tree.pio / "espressif32" / "boards").mkdir(parents=True, exist_ok=True)
        self.assertEqual(vb.validate_board(board, self.tree.pio).status, vb.UNMATCHED)

    def test_board_without_platform_field(self) -> None:
        board = write_board(self.tree.boards, "mystery", None, build=OUR_BUILD)
        self.assertEqual(vb.validate_board(board, self.tree.pio).status, vb.NO_PLATFORM)


# --- end-to-end main() ------------------------------------------------------


class MainTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tree = Tree()
        self.addCleanup(self.tree.close)

    def _clean_pair(self, platform: str = "espressif32", board_id: str = "esp32dev") -> None:
        write_pio_board(self.tree.pio, platform, board_id, build=PIO_BUILD, upload={"protocol": "esptool"})
        write_board(self.tree.boards, board_id, platform, build=OUR_BUILD, upload={"protocol": "esptool"})

    def test_clean_board_exits_zero(self) -> None:
        self._clean_pair()
        rc, out, _err = self.tree.run()
        self.assertEqual(rc, 0)
        self.assertIn("All checked boards match PlatformIO definitions.", out)
        self.assertIn("1 compared, 1 passed", out)

    def test_drift_exits_one_and_names_the_field(self) -> None:
        write_pio_board(self.tree.pio, "espressif32", "esp32dev", build=PIO_BUILD)
        write_board(self.tree.boards, "esp32dev", "espressif32", build={**OUR_BUILD, "f_cpu": "1L"})
        rc, out, _err = self.tree.run()
        self.assertEqual(rc, 1)
        self.assertIn("have drifted from PlatformIO definitions", out)
        self.assertIn("build.f_cpu", out)

    def test_no_platform_installed_is_an_error(self) -> None:
        # RED before the fix: an empty platforms dir meant every board was
        # "skipped" and the run still printed "All checked boards match".
        write_board(self.tree.boards, "esp32dev", "espressif32", build=OUR_BUILD)
        rc, out, err = self.tree.run()
        self.assertEqual(rc, 1)
        self.assertNotIn("All checked boards match", out)
        self.assertIn("Nothing was verified", err)

    def test_installed_platform_with_no_matching_boards_is_an_error(self) -> None:
        write_pio_board(self.tree.pio, "atmelavr", "uno", build=PIO_BUILD)
        write_board(self.tree.boards, "esp32dev", "espressif32", build=OUR_BUILD)
        rc, _out, err = self.tree.run()
        self.assertEqual(rc, 1)
        self.assertIn("Nothing was verified", err)

    def test_new_unmatched_board_fails_and_points_at_the_baseline(self) -> None:
        self._clean_pair()
        write_board(self.tree.boards, "brand-new-board", "espressif32", build=OUR_BUILD)
        rc, out, _err = self.tree.run()
        self.assertEqual(rc, 1)
        self.assertIn("no upstream brand-new-board.json", out)
        self.assertIn("ci/validate_boards_baseline.json", out)
        self.assertIn("1 NEW-unmatched", out)

    def test_baselined_unmatched_board_is_reported_and_passes(self) -> None:
        self._clean_pair()
        write_board(self.tree.boards, "esp32-c5-devkitc-1", "espressif32", build=OUR_BUILD)
        write_json(self.tree.playform, {"schema": 1, "boards": {"espressif32": ["esp32-c5-devkitc-1"]}})
        rc, out, _err = self.tree.run()
        self.assertEqual(rc, 0)
        self.assertIn("1 known-unmatched", out)
        self.assertIn("0 NEW-unmatched", out)
        self.assertIn("espressif32=1", out)

    def test_baseline_for_another_platform_does_not_cover_it(self) -> None:
        self._clean_pair()
        write_board(self.tree.boards, "esp32-c5-devkitc-1", "espressif32", build=OUR_BUILD)
        write_json(self.tree.playform, {"schema": 1, "boards": {"ststm32": ["esp32-c5-devkitc-1"]}})
        rc, _out, _err = self.tree.run()
        self.assertEqual(rc, 1)

    def test_malformed_baseline_is_a_hard_error(self) -> None:
        self._clean_pair()
        self.tree.playform.write_text(json.dumps({"schema": 1, "boards": ["not-a-mapping"]}), encoding="utf-8")
        with self.assertRaises(ValueError):
            self.tree.run()

    def test_fbuild_native_board_is_tallied_not_compared(self) -> None:
        self._clean_pair()
        write_board(self.tree.boards, "lpc845", "nxplpc", build=OUR_BUILD)
        rc, out, _err = self.tree.run()
        self.assertEqual(rc, 0)
        self.assertIn("1 fbuild-native", out)
        self.assertIn("lpc845", out)

    def test_broken_board_json_is_counted_not_validated(self) -> None:
        self._clean_pair()
        (self.tree.boards / "broken.json").write_text("{not json", encoding="utf-8")
        rc, out, _err = self.tree.run()
        self.assertEqual(rc, 0)
        self.assertIn("unreadable board JSON", out)

    def test_platform_filter_rejects_missing_platform(self) -> None:
        self._clean_pair()
        rc, _out, err = self.tree.run("--platforms", "espressif32,teensy")
        self.assertEqual(rc, 1)
        self.assertIn("requested platforms not installed: teensy", err)

    def test_platform_filter_narrows_the_run(self) -> None:
        self._clean_pair("espressif32", "esp32dev")
        self._clean_pair("atmelavr", "uno")
        rc, out, _err = self.tree.run("--platforms", "atmelavr")
        self.assertEqual(rc, 0)
        self.assertIn("1 compared, 1 passed", out)

    def test_unknown_argument_exits_one(self) -> None:
        rc, _out, err = self.tree.run("--nope")
        self.assertEqual(rc, 1)
        self.assertIn("Unknown argument: --nope", err)

    def test_missing_directories_are_errors(self) -> None:
        absent = Path(self.tree.root) / "absent"
        self.tree.boards = absent
        rc, _out, err = self.tree.run()
        self.assertEqual(rc, 1)
        self.assertIn("board assets not found", err)
        self.tree.boards = Path(self.tree.root) / "assets" / "json"
        self.tree.boards.mkdir(parents=True, exist_ok=True)
        self.tree.pio = absent
        rc, _out, err = self.tree.run()
        self.assertEqual(rc, 1)
        self.assertIn("PlatformIO platforms not found", err)


# --- the shipped baseline must describe this repo ---------------------------


class ShippedBaselineTests(unittest.TestCase):
    def setUp(self) -> None:
        self.baseline = vb.load_baseline()
        self.assertTrue(self.baseline, "ci/validate_boards_baseline.json is missing or empty")

    def test_every_baselined_id_still_exists_as_an_asset(self) -> None:
        # A baseline entry for a deleted board silently widens the hole.
        assets = {p.stem: json.loads(p.read_text(encoding="utf-8")) for p in ASSETS.glob("*.json")}
        for platform, board_ids in self.baseline.items():
            for board_id in board_ids:
                self.assertIn(board_id, assets, f"baseline lists {platform}:{board_id} which is not an asset")
                self.assertEqual(assets[board_id].get("platform"), platform)

    def test_baseline_does_not_duplicate_the_fbuild_native_list(self) -> None:
        # Those are excluded before the upstream lookup, so an entry would be dead.
        for board_ids in self.baseline.values():
            self.assertFalse(set(board_ids) & vb.FBUILD_NATIVE_BOARDS)

    def test_baseline_platforms_are_the_pinned_ones(self) -> None:
        text = WORKFLOW.read_text(encoding="utf-8")
        pinned = {
            line.split("PLATFORMIO_")[1].split("_VERSION")[0].lower()
            for line in text.splitlines()
            if line.strip().startswith("PLATFORMIO_") and "_VERSION:" in line and "CORE" not in line
        }
        self.assertTrue(pinned, "could not read the platform pins out of validate-boards.yml")
        self.assertLessEqual(set(self.baseline), pinned)

    def test_baseline_entries_are_sorted_and_unique(self) -> None:
        doc = json.loads(vb.BASELINE_PATH.read_text(encoding="utf-8"))
        for platform, board_ids in doc["boards"].items():
            self.assertEqual(board_ids, sorted(set(board_ids)), platform)


if __name__ == "__main__":
    unittest.main()

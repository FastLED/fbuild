"""Tests for ci/check_python_complexity.py (FastLED/fbuild#1639).

The ratchet had no regression test: the workflow ran the script, so nobody
would notice a loosened ceiling, a dropped rule, or a file-selection change
that quietly stopped covering the scripts normal discovery skips.  These tests
drive the production command against synthetic modules so both faces fail
loudly.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_python_complexity import (  # noqa: E402
    RatchetError,
    check,
    ruff_command,
    tracked_python_files,
)

ROOT = Path(__file__).resolve().parents[1]

# Eleven independent decisions: mccabe reports 1 + 11, above the pinned 10.
OFFENDER = """\
def classify(value):
    if value == 0:
        return "zero"
    elif value == 1:
        return "one"
    elif value == 2:
        return "two"
    elif value == 3:
        return "three"
    elif value == 4:
        return "four"
    elif value == 5:
        return "five"
    elif value == 6:
        return "six"
    elif value == 7:
        return "seven"
    elif value == 8:
        return "eight"
    elif value == 9:
        return "nine"
    elif value == 10:
        return "ten"
    return "many"
"""

CLEAN = """\
def classify(value):
    names = {0: "zero", 1: "one"}
    return names.get(value, "many")
"""

# The suppression is legal syntax but suppresses nothing at this complexity.
OBSOLETE_SUPPRESSION = """\
def classify(value):  # noqa: C901
    return str(value)
"""


class RatchetCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="python-ratchet-")
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        # Copy the repository's real config so the test cannot drift from the
        # policy it is guarding.
        (self.root / "pyproject.toml").write_bytes((ROOT / "pyproject.toml").read_bytes())

    def run_ruff(self, name: str, body: str) -> subprocess.CompletedProcess[str]:
        (self.root / name).write_text(body, encoding="utf-8")
        return subprocess.run(
            ruff_command([name]),
            cwd=self.root,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_clean_module_passes(self) -> None:
        result = self.run_ruff("clean.py", CLEAN)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_function_above_the_ceiling_fails(self) -> None:
        result = self.run_ruff("offender.py", OFFENDER)
        self.assertNotEqual(result.returncode, 0, "C901 must reject complexity 12")
        self.assertIn("C901", result.stdout + result.stderr)

    def test_obsolete_suppression_fails(self) -> None:
        result = self.run_ruff("obsolete.py", OBSOLETE_SUPPRESSION)
        self.assertNotEqual(
            result.returncode, 0, "an unnecessary noqa must fail the ratchet"
        )
        self.assertIn("RUF100", result.stdout + result.stderr)


class PolicyCase(unittest.TestCase):
    def test_policy_is_not_loosened(self) -> None:
        config = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
        lint = config["tool"]["ruff"]["lint"]
        self.assertEqual(sorted(lint["select"]), ["C901", "RUF100"])
        self.assertEqual(lint["mccabe"]["max-complexity"], 10)

    def test_tracked_files_cover_discovery_excluded_scripts(self) -> None:
        paths = tracked_python_files(ROOT)
        self.assertTrue(paths, "the ratchet must find tracked Python files")
        self.assertTrue(all(path.endswith(".py") for path in paths), paths[:3])
        self.assertEqual(len(paths), len(set(paths)), "git ls-files must not repeat")
        # Not a `test_*.py` module, so unittest discovery alone would miss it.
        self.assertIn("ci/build_backend.py", paths)

    def test_empty_repository_is_an_error(self) -> None:
        with tempfile.TemporaryDirectory(prefix="python-ratchet-empty-") as raw:
            root = Path(raw)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            with self.assertRaises(RatchetError):
                tracked_python_files(root)

    def test_repository_is_currently_clean(self) -> None:
        self.assertEqual(check(ROOT), 0, "origin/main must satisfy its own ratchet")


if __name__ == "__main__":
    unittest.main()

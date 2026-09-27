"""Contract tests for the local Rust lint entry point."""

import runpy
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent


class LintEntrypointTests(unittest.TestCase):
    def setUp(self) -> None:
        self.module = runpy.run_path(str(ROOT / "lint"))
        self.commands: list[list[str]] = []

    def run_command(self, args: list[str]) -> int:
        self.commands.append(args)
        return 0

    def test_full_lint_runs_workspace_dylint_after_clippy(self) -> None:
        main = self.module["main"]
        with patch.object(sys, "argv", ["lint"]), patch.dict(main.__globals__, {"run": self.run_command}):
            self.assertEqual(0, main())
        self.assertEqual([sys.executable, "-m", "ci.run_dylint"], self.commands[-1])
        self.assertEqual("clippy", self.commands[-2][2])

    def test_fix_mode_still_runs_dylint(self) -> None:
        main = self.module["main"]
        with patch.object(sys, "argv", ["lint", "--fix"]), patch.dict(main.__globals__, {"run": self.run_command}):
            self.assertEqual(0, main())
        self.assertEqual([sys.executable, "-m", "ci.run_dylint"], self.commands[-1])

    def test_single_file_runs_dylint_too(self) -> None:
        main = self.module["main"]
        file_arg = ROOT / "crates" / "fbuild-core" / "src" / "lib.rs"
        with patch.object(sys, "argv", ["lint", str(file_arg)]), patch.dict(main.__globals__, {"run": self.run_command}):
            self.assertEqual(0, main())
        self.assertEqual([sys.executable, "-m", "ci.run_dylint", "--package", "fbuild-core"], self.commands[-1])


if __name__ == "__main__":
    unittest.main()

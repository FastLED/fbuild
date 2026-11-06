"""Contract tests for the local Rust lint entry point."""

import runpy
import sys
import unittest
from pathlib import Path
from typing import Callable
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent


class LintEntrypointTests(unittest.TestCase):
    def setUp(self) -> None:
        self.module = runpy.run_path(str(ROOT / "lint"))
        self.commands: list[list[str]] = []

    def run_command(self, args: list[str]) -> int:
        self.commands.append(args)
        return 0

    def failing_command(self, needle: str, code: int) -> Callable[[list[str]], int]:
        """Every command succeeds except the ones mentioning `needle`."""

        def run(args: list[str]) -> int:
            self.commands.append(args)
            return code if any(needle in part for part in args) else 0

        return run

    def main_with(self, run: Callable[[list[str]], int], argv: list[str]) -> int:
        main = self.module["main"]
        with patch.object(sys, "argv", argv), patch.dict(main.__globals__, {"run": run}):
            return main()

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

    def test_policy_precheck_failure_blocks_every_rust_step(self) -> None:
        """A failing toolchain-pin or boundary check must stop the run before Rust."""
        rc = self.main_with(self.failing_command("check_rust_toolchain_pins.py", 1), ["lint"])
        self.assertEqual(1, rc)
        self.assertEqual(1, len(self.commands), self.commands)
        self.assertIn("check_rust_toolchain_pins.py", self.commands[0][-1])

    def test_format_failure_skips_clippy_and_dylint(self) -> None:
        rc = self.main_with(self.failing_command("fmt", 1), ["lint"])
        self.assertEqual(1, rc)
        self.assertFalse(any("clippy" in part for cmd in self.commands for part in cmd), self.commands)
        self.assertFalse(any("ci.run_dylint" in part for cmd in self.commands for part in cmd), self.commands)

    def test_clippy_failure_skips_dylint_and_is_propagated(self) -> None:
        """A green summary over a red clippy would let broken code through."""
        rc = self.main_with(self.failing_command("clippy", 2), ["lint"])
        self.assertEqual(2, rc)
        self.assertFalse(
            any("ci.run_dylint" in part for cmd in self.commands for part in cmd),
            self.commands,
        )

    def test_dylint_failure_is_propagated(self) -> None:
        rc = self.main_with(self.failing_command("ci.run_dylint", 3), ["lint"])
        self.assertEqual(3, rc)

    def test_missing_file_fails_before_running_anything(self) -> None:
        rc = self.main_with(self.run_command, ["lint", str(ROOT / "crates" / "gone.rs")])
        self.assertEqual(1, rc)
        self.assertEqual([], self.commands)


if __name__ == "__main__":
    unittest.main()

"""Unit tests for the RUST-033 local lint lane."""

from __future__ import annotations

import unittest
from unittest import mock

from ci import local_lint_gate


class LintCommandTest(unittest.TestCase):
    def test_command_runs_the_full_lint_phase_with_cross_dylint(self) -> None:
        command = local_lint_gate.lint_command()
        self.assertEqual(command, ["soldr", "lint", "rust", "--cross-dylint"])

    def test_run_lint_refuses_a_non_linux_host(self) -> None:
        with mock.patch.object(local_lint_gate.platform, "system", return_value="Darwin"):
            with self.assertRaises(SystemExit) as raised:
                local_lint_gate.run_lint()
        self.assertEqual(raised.exception.code, 1)


if __name__ == "__main__":
    unittest.main()

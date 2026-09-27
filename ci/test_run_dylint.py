"""Tests for local Dylint orchestration without downloading toolchains."""

import os
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

from ci import run_dylint


class RunDylintTests(unittest.TestCase):
    def test_prepares_prebuilt_tools_and_lints_workspace(self) -> None:
        commands: list[list[str]] = []

        def run(args: list[str], **_kwargs):
            self.assertNotIn("RUSTUP_TOOLCHAIN", _kwargs["env"])
            commands.append(args)
            return subprocess.CompletedProcess(args, 0)

        with (
            patch.dict(os.environ, {"CARGO_HOME": "", "FBUILD_PLATFORM_BOUNDARY_OBSERVED": "stale", "RUSTUP_TOOLCHAIN": "1.95.0"}),
            patch.object(Path, "home", return_value=Path("/tmp/fbuild-test-home")),
            patch.object(run_dylint, "activate"),
            patch.object(run_dylint.subprocess, "run", side_effect=run),
        ):
            self.assertEqual(0, run_dylint.main([]))
        self.assertEqual(["soldr", "dylint", "prepare"], commands[0])
        self.assertEqual(
            ["soldr", "dylint", "--all", "--", "--workspace", "--all-targets"],
            commands[1],
        )

    def test_failed_dylint_returns_nonzero(self) -> None:
        with (
            patch.object(run_dylint, "activate"),
            patch.object(run_dylint.subprocess, "run", side_effect=[
                subprocess.CompletedProcess([], 0), subprocess.CompletedProcess([], 17)
            ]),
        ):
            self.assertEqual(17, run_dylint.main([]))

    def test_package_mode_lints_only_selected_crate_without_global_observation(self) -> None:
        calls = []

        def run(args: list[str], **kwargs):
            calls.append((args, kwargs))
            return subprocess.CompletedProcess(args, 0)

        with (
            patch.dict(os.environ, {"FBUILD_PLATFORM_BOUNDARY_OBSERVED": "stale"}),
            patch.object(run_dylint, "activate"),
            patch.object(run_dylint.subprocess, "run", side_effect=run),
        ):
            self.assertEqual(0, run_dylint.main(["--package", "fbuild-core"]))
        self.assertEqual(["--package", "fbuild-core", "--all-targets"], calls[1][0][-3:])
        self.assertNotIn("FBUILD_PLATFORM_BOUNDARY_OBSERVED", calls[1][1]["env"])


if __name__ == "__main__":
    unittest.main()

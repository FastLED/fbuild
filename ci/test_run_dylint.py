"""Tests for pinned local Dylint orchestration without compiling toolchains."""

import os
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

from ci import run_dylint


class RunDylintTests(unittest.TestCase):
    def test_default_cargo_home_does_not_depend_on_rustup_binary_location(self) -> None:
        with (
            patch.dict(os.environ, {"CARGO_HOME": ""}),
            patch.object(Path, "home", return_value=Path("/tmp/fbuild-test-home")),
            patch.object(run_dylint, "activate"),
            patch.object(run_dylint, "ensure_tools", return_value=Path("/tmp/fbuild-test-cargo-dylint")),
            patch.object(run_dylint, "ensure_driver"),
            patch.object(run_dylint.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)) as runner,
        ):
            self.assertEqual(0, run_dylint.main([]))
        self.assertEqual("/tmp/fbuild-test-home/.cargo", runner.call_args.kwargs["env"]["CARGO_HOME"])

    def test_matching_driver_is_reused(self) -> None:
        host = "host: x86_64-unknown-linux-gnu\n"
        version = "nightly-2026-04-16-x86_64-unknown-linux-gnu 6.0.1\n"
        with (
            patch.object(run_dylint.subprocess, "check_output", side_effect=[host, version]),
            patch.object(Path, "is_file", return_value=True),
            patch.object(run_dylint.subprocess, "run") as runner,
        ):
            run_dylint.ensure_driver({})
        runner.assert_not_called()

    def test_full_workspace_uses_pinned_binary(self) -> None:
        binary = Path("/tmp/fbuild-test-cargo-dylint")
        commands: list[tuple[list[str], dict[str, str] | None]] = []

        def run(args: list[str], **kwargs):
            commands.append((args, kwargs.get("env")))
            return subprocess.CompletedProcess(args, 0)

        with (
            patch.dict(os.environ, {"CARGO_HOME": "/tmp/fbuild-test-cargo-home"}),
            patch.object(run_dylint, "activate"),
            patch.object(run_dylint, "ensure_tools", return_value=binary),
            patch.object(run_dylint, "ensure_driver"),
            patch.object(run_dylint.subprocess, "run", side_effect=run),
        ):
            self.assertEqual(0, run_dylint.main([]))

        self.assertEqual(
            [
                "soldr", "rustup", "run", "nightly-2026-04-16", str(binary),
                "dylint", "--all", "--", "--workspace", "--all-targets",
            ],
            commands[0][0],
        )
        env = commands[0][1]
        assert env is not None
        self.assertEqual(str(run_dylint.DRIVER_ROOT), env["DYLINT_DRIVER_PATH"])
        self.assertEqual(1, len(commands))

    def test_failed_dylint_returns_nonzero(self) -> None:
        with (
            patch.dict(os.environ, {"CARGO_HOME": "/tmp/fbuild-test-cargo-home"}),
            patch.object(run_dylint, "activate"),
            patch.object(run_dylint, "ensure_tools", return_value=Path("/tmp/fbuild-test-cargo-dylint")),
            patch.object(run_dylint, "ensure_driver"),
            patch.object(run_dylint.subprocess, "run", return_value=subprocess.CompletedProcess([], 17)) as runner,
        ):
            self.assertEqual(17, run_dylint.main([]))
        runner.assert_called_once()

    def test_package_mode_lints_only_selected_crate_without_global_observation(self) -> None:
        with (
            patch.dict(os.environ, {"CARGO_HOME": "/tmp/fbuild-test-cargo-home"}),
            patch.object(run_dylint, "activate"),
            patch.object(run_dylint, "ensure_tools", return_value=Path("/tmp/fbuild-test-cargo-dylint")),
            patch.object(run_dylint, "ensure_driver"),
            patch.object(run_dylint.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)) as runner,
        ):
            self.assertEqual(0, run_dylint.main(["--package", "fbuild-core"]))
        runner.assert_called_once()
        args = runner.call_args.args[0]
        self.assertEqual(["--package", "fbuild-core", "--all-targets"], args[-3:])
        env = runner.call_args.kwargs["env"]
        self.assertNotIn("FBUILD_PLATFORM_BOUNDARY_OBSERVED", env)
        self.assertNotIn("fbuild_platform_boundary_observation_run_", env.get("RUSTFLAGS", ""))


if __name__ == "__main__":
    unittest.main()

"""Tests for local Dylint orchestration without downloading toolchains.

``ci/run_dylint.py`` is what the developer-facing `lint` entry point runs last
(``uv run --script lint``), and what CI's dylint lanes use to drive the custom
lints.  It is a two-step gate: ``soldr dylint prepare`` fetches the pinned
nightly + driver, then ``soldr dylint --all`` runs the lints over the workspace
(or one edited crate).

The safety properties are all in the environment it hands to soldr and in the
exit code it returns, so the tests drive those directly: a failed prepare must
stop before the lint and report its own code, a caller's CARGO_HOME must survive,
the CI-only observation ledger must be dropped, and every command must run from
the repo root after ``activate()`` put soldr on PATH.
"""

import contextlib
import io
import os
import subprocess
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest.mock import patch

from ci import run_dylint


class Harness:
    """Record the ordered effect of ``activate()`` and every subprocess call."""

    def __init__(self, returncodes: list[int] | None = None) -> None:
        self.events: list[str] = []
        self.calls: list[dict] = []
        self.returncodes = list(returncodes or [])
        self._stack = contextlib.ExitStack()

    def __enter__(self) -> "Harness":
        self._stack.enter_context(
            patch.object(run_dylint, "activate", lambda: self.events.append("activate"))
        )
        self._stack.enter_context(patch.object(run_dylint.subprocess, "run", self._run))
        return self

    def __exit__(self, *exc_info) -> bool:
        return self._stack.__exit__(*exc_info)

    def _run(self, args, **kwargs):
        self.events.append("run")
        self.calls.append({"args": list(args), "kwargs": kwargs})
        code = self.returncodes.pop(0) if self.returncodes else 0
        return subprocess.CompletedProcess(list(args), code)

    @property
    def argv(self) -> list[list[str]]:
        return [call["args"] for call in self.calls]

    @property
    def envs(self) -> list[dict[str, str]]:
        return [call["kwargs"]["env"] for call in self.calls]


PREPARE = ["soldr", "dylint", "prepare"]
WORKSPACE_LINT = ["soldr", "dylint", "--all", "--", "--workspace", "--all-targets"]


class RunDylintTests(unittest.TestCase):
    def test_prepares_prebuilt_tools_and_lints_workspace(self) -> None:
        with (
            patch.dict(os.environ, {"CARGO_HOME": "", "FBUILD_PLATFORM_BOUNDARY_OBSERVED": "stale", "RUSTUP_TOOLCHAIN": "1.95.0"}),
            patch.object(Path, "home", return_value=Path("/tmp/fbuild-test-home")),
            Harness() as harness,
        ):
            self.assertEqual(0, run_dylint.main([]))
        self.assertEqual([PREPARE, WORKSPACE_LINT], harness.argv)
        self.assertNotIn("RUSTUP_TOOLCHAIN", harness.envs[0])

    def test_failed_dylint_returns_nonzero(self) -> None:
        with Harness([0, 17]) as harness:
            self.assertEqual(17, run_dylint.main([]))
        self.assertEqual([PREPARE, WORKSPACE_LINT], harness.argv)

    def test_package_mode_lints_only_selected_crate_without_global_observation(self) -> None:
        with (
            patch.dict(os.environ, {"FBUILD_PLATFORM_BOUNDARY_OBSERVED": "stale"}),
            Harness() as harness,
        ):
            self.assertEqual(0, run_dylint.main(["--package", "fbuild-core"]))
        self.assertEqual(["--package", "fbuild-core", "--all-targets"], harness.argv[1][-3:])
        self.assertNotIn("FBUILD_PLATFORM_BOUNDARY_OBSERVED", harness.envs[1])

    def test_failed_prepare_returns_its_code_and_never_lints(self) -> None:
        """A prepare that cannot fetch the driver must not report a lint verdict."""
        stderr = io.StringIO()
        with Harness([3]) as harness, redirect_stderr(stderr):
            self.assertEqual(3, run_dylint.main([]))
        self.assertEqual([PREPARE], harness.argv)
        message = stderr.getvalue()
        self.assertIn("prepare", message)
        self.assertIn("3", message)

    def test_successful_prepare_does_not_write_to_stderr(self) -> None:
        stderr = io.StringIO()
        with Harness() as harness, redirect_stderr(stderr):
            self.assertEqual(0, run_dylint.main([]))
        self.assertEqual("", stderr.getvalue())
        self.assertEqual([PREPARE, WORKSPACE_LINT], harness.argv)

    def test_missing_cargo_home_is_derived_from_the_home_directory(self) -> None:
        with (
            patch.dict(os.environ, {"CARGO_HOME": ""}),
            patch.object(Path, "home", return_value=Path("/tmp/fbuild-test-home")),
            Harness() as harness,
        ):
            run_dylint.main([])
        self.assertEqual(
            [str(Path("/tmp/fbuild-test-home") / ".cargo")] * 2,
            [env["CARGO_HOME"] for env in harness.envs],
        )

    def test_existing_cargo_home_is_not_overwritten(self) -> None:
        """soldr resolves its post-install self-check against CARGO_HOME."""
        with patch.dict(os.environ, {"CARGO_HOME": "/opt/cargo"}), Harness() as harness:
            run_dylint.main([])
        self.assertEqual(["/opt/cargo", "/opt/cargo"], [env["CARGO_HOME"] for env in harness.envs])

    def test_workspace_mode_drops_the_observation_ledger(self) -> None:
        """Locally the ledger must be dropped so Cargo fingerprints are reusable."""
        with (
            patch.dict(os.environ, {"FBUILD_PLATFORM_BOUNDARY_OBSERVED": "stale"}),
            Harness() as harness,
        ):
            run_dylint.main([])
        self.assertEqual(2, len(harness.envs))
        for env in harness.envs:
            self.assertNotIn("FBUILD_PLATFORM_BOUNDARY_OBSERVED", env)

    def test_rustup_toolchain_is_dropped_in_package_mode(self) -> None:
        with (
            patch.dict(os.environ, {"RUSTUP_TOOLCHAIN": "1.95.0"}),
            Harness() as harness,
        ):
            run_dylint.main(["--package", "fbuild-core"])
        for env in harness.envs:
            self.assertNotIn("RUSTUP_TOOLCHAIN", env)

    def test_every_command_runs_from_the_repo_root(self) -> None:
        with Harness() as harness:
            run_dylint.main([])
        self.assertEqual([run_dylint.ROOT, run_dylint.ROOT], [call["kwargs"]["cwd"] for call in harness.calls])

    def test_activate_runs_before_the_first_subprocess(self) -> None:
        """soldr only resolves once activate() has put it on PATH."""
        with Harness() as harness:
            run_dylint.main([])
        self.assertEqual(["activate", "run", "run"], harness.events)


if __name__ == "__main__":
    unittest.main()

"""Cache-save classification and daemon-death naming must both hold."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from ci.test_cache_status import DaemonDeath, TestEvidence


class TestCacheStatusTests(unittest.TestCase):
    def evidence(self, *lines: str) -> TestEvidence:
        result = TestEvidence()
        for line in lines:
            result = result.observe(line)
        return result

    def test_completed_assertion_failure(self):
        result = self.evidence(
            '{"reason":"build-finished","success":true}',
            "test result: FAILED. 0 passed; 1 failed;",
            "    0.59 error: test failed, to rerun pass `--lib`",
            "    0.60 soldr[cache] probe [MISS]",
        )
        self.assertTrue(result.assertion_failure_only(101))
        self.assertFalse(result.assertion_failure_only(-15))
        self.assertFalse(result.assertion_failure_only(0))

    def test_compiler_failure_and_incomplete_output_reject(self):
        for build in (
            '{"reason":"build-finished","success":false}',
            "",
            '{"reason":"compiler-message","message":{"level":"error"}}',
        ):
            result = self.evidence(build, "test result: FAILED.", "error: test failed")
            self.assertFalse(result.assertion_failure_only(101))

    def test_doctest_compilation_failure_rejects(self):
        result = self.evidence(
            '{"reason":"build-finished","success":true}',
            "Couldn't compile the test.",
            "test result: FAILED.",
            "error: doctest failed, to rerun pass `--doc`",
        )
        self.assertFalse(result.assertion_failure_only(101))

    def test_unrelated_terminal_failure_rejects(self):
        result = self.evidence(
            '{"reason":"build-finished","success":true}',
            "test result: FAILED.",
            "error: could not compile `x`",
        )
        self.assertFalse(result.assertion_failure_only(101))


RELAY_RESET_LINE = (
    "471.71 soldr: SESSION relay closed before Exit; the daemon closed the compile "
    "before any output -- retrying once (see `soldr logs paths`; soldr#3377)"
)
LOST_LINE = (
    "warning: tree-sitter-cpp@0.23.4: zccache-soldr: dispatch failed: broker SESSION "
    "compile failed: soldr: ERROR: lost the connection to the soldr daemon "
    "(route soldr-daemon-7f93f9787aa90e99a66b7496369b535c) after 11.1s: "
    "Connection reset by peer (os error 104)"
)


class DaemonDeathTests(unittest.TestCase):
    """A daemon death must be named as infra, never as a diff failure (#2178)."""

    def evidence(self, *lines: str) -> DaemonDeath:
        result = DaemonDeath()
        for line in lines:
            result = result.observe(line)
        return result

    def test_clean_log_blames_nothing(self):
        result = self.evidence(
            '{"reason":"build-finished","success":true}', "test result: ok."
        )
        self.assertEqual("", result.verdict())
        self.assertEqual("", result.route())

    def test_relay_reset_is_classified(self):
        result = self.evidence(RELAY_RESET_LINE)
        self.assertIn("relay reset(s)", result.verdict())
        self.assertIn("soldr#3377", result.verdict())

    def test_lost_connection_names_the_route(self):
        result = self.evidence(RELAY_RESET_LINE, LOST_LINE)
        self.assertEqual(
            "soldr-daemon-7f93f9787aa90e99a66b7496369b535c", result.route()
        )
        verdict = result.verdict()
        self.assertIn("soldr-daemon-7f93f978", verdict)
        self.assertIn("lost-connection report(s)", verdict)

    def test_forwarded_cargo_warning_keeps_its_route(self):
        result = self.evidence(
            "  473.37   cargo:warning=" + "zccache-soldr: dispatch failed: "
            "soldr: ERROR: " + LOST_LINE.split("soldr: ERROR: ", 1)[1]
        )
        self.assertEqual(1, len(result.lost))
        self.assertEqual(
            "soldr-daemon-7f93f9787aa90e99a66b7496369b535c", result.route()
        )

    def test_repeated_identical_lines_are_counted_once(self):
        result = self.evidence(RELAY_RESET_LINE, RELAY_RESET_LINE)
        self.assertEqual(1, len(result.resets))


class DaemonVerdictWiringTests(unittest.TestCase):
    """The verdict must reach the step summary through the real entry point."""

    def run_wrapper(self, print_lines: list[str]) -> tuple[int, str, str]:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            shim = root / "soldr"
            body = "\n".join(f"echo '{line}'" for line in print_lines)
            shim.write_text(f"#!/bin/sh\n{body}\nexit 101\n")
            shim.chmod(0o755)
            summary = root / "summary.md"
            summary.touch()
            env = dict(
                os.environ,
                PATH=f"{root}{os.pathsep}{os.environ['PATH']}",
                GITHUB_STEP_SUMMARY=str(summary),
            )
            env.pop("GITHUB_OUTPUT", None)
            repo = Path(__file__).resolve().parents[1]
            result = subprocess.run(
                [
                    sys.executable,
                    str(repo / "ci" / "test_cache_status.py"),
                    "--",
                    "soldr",
                    "cargo",
                    "test",
                ],
                cwd=repo,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            return result.returncode, result.stdout, summary.read_text()

    def test_daemon_death_is_annotated_and_summarised(self):
        code, stdout, summary = self.run_wrapper([RELAY_RESET_LINE, LOST_LINE])
        self.assertEqual(101, code)
        self.assertIn("::error::INFRA -", stdout)
        self.assertIn("soldr-daemon-7f93f978", summary)
        self.assertIn("#2178", summary)

    def test_clean_run_writes_no_infra_verdict(self):
        code, stdout, summary = self.run_wrapper(["test result: ok. 3 passed"])
        self.assertEqual(101, code)
        self.assertNotIn("INFRA", stdout)
        self.assertEqual("", summary)

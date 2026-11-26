"""Cache-save classification and daemon-death naming must both hold."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from ci.test_cache_status import (
    DaemonDeath,
    RetryDecision,
    TestEvidence,
    retry_decision,
)


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

    def test_failed_build_record_is_not_a_compiler_failure(self):
        """A failed build-finished record alone says nothing about the code.

        The daemon kill ends the build script with exit 101, so cargo always
        emits this record; treating it as a compiler failure made the infra
        verdict veto itself.
        """

        result = self.evidence('{"reason":"build-finished","success":false}')
        self.assertTrue(result.build_failed)
        self.assertFalse(result.build_finished)
        self.assertFalse(result.compiler_failed)
        self.assertFalse(result.tests_failed)

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

    def test_one_kill_reported_twice_is_counted_once(self):
        """cargo prints one kill twice: its own `warning:` line, then a forwarded copy.

        Shape captured from the real Dell job 139429807645 (2026-11-25T18:47:30Z):
        the same event at +229.60s as `warning: ring@0.17.14: ...` and at +229.61s
        as `cargo:warning=...`. Two renderings, one kill -- a count of 2 would
        overstate how many compiles the daemon actually dropped.
        """

        body = LOST_LINE[LOST_LINE.index("zccache-soldr:") :]
        forwarded = "  229.61   cargo:warning=" + body
        result = self.evidence(LOST_LINE, forwarded)
        self.assertEqual(1, len(result.lost))
        self.assertIn("1 lost-connection report(s)", result.verdict())

    def test_real_dell_log_reports_one_kill(self):
        """The captured Dell log: 3 identical relay lines + both lost renderings."""

        body = LOST_LINE[LOST_LINE.index("zccache-soldr:") :]
        forwarded = "  229.61   cargo:warning=" + body
        result = self.evidence(
            RELAY_RESET_LINE, RELAY_RESET_LINE, RELAY_RESET_LINE, LOST_LINE, forwarded
        )
        verdict = result.verdict()
        self.assertIn("1 relay reset(s) (soldr#3377)", verdict)
        self.assertIn("1 lost-connection report(s)", verdict)
        self.assertIn("soldr-daemon-7f93f978", verdict)


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

    def test_real_test_failure_vetoes_the_infra_claim(self):
        """A failed test is never "not a code failure" (#2186 merge group 45701476278).

        Shape captured live: 8 relay resets, a lost connection, AND
        `test result: FAILED. 5 passed; 1 failed`. The daemon did drop compiles,
        but the runner's own failure stands -- the annotation must not excuse it.
        """

        code, stdout, summary = self.run_wrapper(
            [
                RELAY_RESET_LINE,
                LOST_LINE,
                "test result: FAILED. 5 passed; 1 failed; 3 ignored; 0 measured",
            ]
        )
        self.assertEqual(101, code)
        self.assertNotIn("not a code failure", stdout)
        self.assertIn("NOT proven infra", stdout)
        # The daemon hit is still named, so the operator sees both facts.
        self.assertIn("soldr-daemon-7f93f978", stdout)
        self.assertEqual("", summary)

    def test_daemon_kill_own_build_failure_is_still_infra(self):
        """The kill's own exit 101 must not veto the kill's classification.

        Shape captured live (#2186 push run 45707669541, job 139533914957): relay
        resets and a lost connection, then cargo reports a failed
        ``build-finished`` record and cc-rs an exit 101 -- with no rustc
        diagnostic and no failed test. The earlier veto read that record as a
        compiler failure and printed "NOT proven infra" for a job that had no
        code failure at all.
        """

        code, stdout, summary = self.run_wrapper(
            [
                RELAY_RESET_LINE,
                LOST_LINE,
                '{"reason":"build-finished","success":false}',
                "error occurred in cc-rs: command did not execute successfully",
            ]
        )
        self.assertEqual(101, code)
        self.assertNotIn("NOT proven infra", stdout)
        self.assertIn("::error::INFRA -", stdout)
        self.assertIn("soldr-daemon-7f93f978", summary)

    def test_real_compiler_diagnostic_still_vetoes(self):
        """A genuine rustc error keeps the veto even when no test ran."""

        code, stdout, summary = self.run_wrapper(
            [
                RELAY_RESET_LINE,
                '{"reason":"compiler-message","message":{"level":"error"}}',
            ]
        )
        self.assertEqual(101, code)
        self.assertIn("NOT proven infra", stdout)
        self.assertEqual("", summary)

    def test_clean_run_writes_no_infra_verdict(self):
        code, stdout, summary = self.run_wrapper(["test result: ok. 3 passed"])
        self.assertEqual(101, code)
        self.assertNotIn("INFRA", stdout)
        self.assertEqual("", summary)


class DaemonRetryDecisionTests(unittest.TestCase):
    """The retry is bounded on every axis, so a real failure is never re-run (#2178)."""

    def decision(
        self,
        *,
        daemon: DaemonDeath | None = None,
        evidence: TestEvidence | None = None,
        returncode: int = 101,
        elapsed_seconds: float = 60.0,
        attempts: int = 1,
        enabled: bool = True,
        budget_seconds: float = 600.0,
    ) -> RetryDecision:
        return retry_decision(
            daemon=daemon
            if daemon is not None
            else DaemonDeath().observe(RELAY_RESET_LINE),
            evidence=evidence if evidence is not None else TestEvidence(),
            returncode=returncode,
            elapsed_seconds=elapsed_seconds,
            attempts=attempts,
            enabled=enabled,
            budget_seconds=budget_seconds,
        )

    def test_infra_kill_within_budget_is_retried(self):
        self.assertTrue(self.decision().allowed)

    def test_successful_run_is_never_retried(self):
        self.assertFalse(self.decision(returncode=0).allowed)

    def test_real_test_failure_is_never_retried(self):
        evidence = TestEvidence().observe(
            "test result: FAILED. 5 passed; 1 failed; 3 ignored; 0 measured"
        )
        self.assertFalse(self.decision(evidence=evidence).allowed)

    def test_rustc_diagnostic_is_never_retried(self):
        evidence = TestEvidence().observe(
            '{"reason":"compiler-message","message":{"level":"error"}}'
        )
        self.assertFalse(self.decision(evidence=evidence).allowed)

    def test_clean_log_is_never_retried(self):
        self.assertFalse(self.decision(daemon=DaemonDeath()).allowed)

    def test_budget_exhausted_blocks_the_retry(self):
        self.assertFalse(self.decision(elapsed_seconds=601.0).allowed)

    def test_retry_is_opt_in(self):
        self.assertFalse(self.decision(enabled=False).allowed)

    def test_only_one_retry(self):
        self.assertFalse(self.decision(attempts=2).allowed)


class DaemonRetryWiringTests(unittest.TestCase):
    """The retry must reach the real entry point with a real child process."""

    def run_scripted(
        self,
        first_lines: list[str],
        second_lines: list[str],
        env_extra: dict[str, str],
    ) -> tuple[int, str, int]:
        def block(lines: list[str]) -> str:
            return "cat <<'LINES'\n" + "\n".join(lines) + "\nLINES\n"

        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            counter = root / "count"
            shim = root / "soldr"
            shim.write_text(
                "#!/bin/sh\n"
                'count_file="$SHIM_COUNTER"\n'
                'count=$(cat "$count_file" 2>/dev/null || echo 0)\n'
                "count=$((count + 1))\n"
                'echo "$count" > "$count_file"\n'
                'if [ "$count" -eq 1 ]; then\n' + block(first_lines) + "exit 101\n"
                "fi\n" + block(second_lines) + "exit 0\n"
            )
            shim.chmod(0o755)
            summary = root / "summary.md"
            summary.touch()
            env = dict(
                os.environ,
                PATH=f"{root}{os.pathsep}{os.environ['PATH']}",
                GITHUB_STEP_SUMMARY=str(summary),
                SHIM_COUNTER=str(counter),
                **env_extra,
            )
            env.pop("GITHUB_OUTPUT", None)
            env.pop("FBUILD_CI_DAEMON_DEATH_RETRY", None)
            env.pop("FBUILD_CI_DAEMON_RETRY_BUDGET_SECONDS", None)
            env.update(env_extra)
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
            invocations = int(counter.read_text()) if counter.exists() else 0
            return result.returncode, result.stdout, invocations

    def test_retry_reruns_a_daemon_kill_once_and_turns_green(self):
        code, stdout, invocations = self.run_scripted(
            [RELAY_RESET_LINE, LOST_LINE],
            ["test result: ok. 3 passed"],
            {"FBUILD_CI_DAEMON_DEATH_RETRY": "1"},
        )
        self.assertEqual(0, code)
        self.assertEqual(2, invocations)
        self.assertIn("retrying the command once", stdout)

    def test_retry_is_off_without_the_opt_in(self):
        code, stdout, invocations = self.run_scripted(
            [RELAY_RESET_LINE, LOST_LINE],
            ["test result: ok. 3 passed"],
            {},
        )
        self.assertEqual(101, code)
        self.assertEqual(1, invocations)

    def test_exhausted_budget_blocks_the_retry(self):
        code, _stdout, invocations = self.run_scripted(
            [RELAY_RESET_LINE, LOST_LINE],
            ["test result: ok. 3 passed"],
            {
                "FBUILD_CI_DAEMON_DEATH_RETRY": "1",
                "FBUILD_CI_DAEMON_RETRY_BUDGET_SECONDS": "0",
            },
        )
        self.assertEqual(101, code)
        self.assertEqual(1, invocations)

    def test_real_test_failure_is_not_retried_even_when_enabled(self):
        code, _stdout, invocations = self.run_scripted(
            [
                RELAY_RESET_LINE,
                "test result: FAILED. 5 passed; 1 failed; 3 ignored; 0 measured",
            ],
            ["test result: ok. 3 passed"],
            {"FBUILD_CI_DAEMON_DEATH_RETRY": "1"},
        )
        self.assertEqual(101, code)
        self.assertEqual(1, invocations)

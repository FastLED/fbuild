"""Tests for ci/gate_jobs.py, the `CI summary` aggregator."""

from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import gate_jobs  # noqa: E402


def job(name: str, conclusion: str | None, status: str = "completed") -> dict:
    return {"name": name, "status": status, "conclusion": conclusion}


class BlockingTest(unittest.TestCase):
    def test_every_unsuccessful_conclusion_blocks(self):
        for conclusion in sorted(gate_jobs.UNSUCCESSFUL):
            with self.subTest(conclusion=conclusion):
                self.assertEqual(["linux / Check (ubuntu-latest)"],
                                 gate_jobs.blocking([job("linux / Check (ubuntu-latest)", conclusion)]))

    def test_success_and_skipped_pass(self):
        self.assertEqual([], gate_jobs.blocking([job("a", "success"), job("b", "skipped"), job("c", "neutral")]))

    def test_nested_reusable_job_cancelled_blocks(self):
        # needs.*.result would report the `dylint` call as success here.
        self.assertEqual(["dylint / Dylint Full (Linux builder, all OS targets)"],
                         gate_jobs.blocking([job("dylint / Dylint Full (Linux builder, all OS targets)", "cancelled")]))

    def test_exempt_and_unfinished_jobs_ignored(self):
        jobs = [
            job("CI summary", None, status="in_progress"),
            job("Verify local gate", "failure"),
            job("Verified reuse decision (shadow)", "failure"),
            job("End-of-queue cache janitor", "failure"),
            job("dylint / End-of-queue cache janitor", "failure"),
            job("linux / Check (ubuntu-latest)", None, status="queued"),
        ]
        self.assertEqual([], gate_jobs.blocking(jobs))


class RunJobsTest(unittest.TestCase):
    ENV = {"GITHUB_REPOSITORY": "FastLED/fbuild", "GITHUB_RUN_ID": "7", "GITHUB_RUN_ATTEMPT": "2"}

    def test_lists_this_run_attempt(self):
        with mock.patch.dict(os.environ, self.ENV):
            argv = gate_jobs.jobs_argv()
        self.assertIn("repos/FastLED/fbuild/actions/runs/7/attempts/2/jobs?per_page=30", argv)
        self.assertIn("--paginate", argv)

    def test_paginated_output_keeps_later_failure(self):
        output = '\n'.join(json.dumps(item) for item in [
            job("first page", "success"), job("later page", "failure")
        ])
        with mock.patch.dict(os.environ, self.ENV):
            jobs = gate_jobs.run_jobs(runner=lambda *a, **k: subprocess.CompletedProcess([], 0, output, ""))
        self.assertEqual(["later page"], gate_jobs.blocking(jobs))

    def test_retries_transient_failure(self):
        results = [subprocess.CompletedProcess([], 1, "", "HTTP 502"),
                   subprocess.CompletedProcess([], 0, '{"name": "x", "status": "completed", "conclusion": "success"}\n', "")]
        sleeps: list[float] = []
        with mock.patch.dict(os.environ, self.ENV):
            jobs = gate_jobs.run_jobs(runner=lambda *a, **k: results.pop(0), sleep=sleeps.append)
        self.assertEqual("x", jobs[0]["name"])
        self.assertEqual(1, len(sleeps))

    def test_gives_up_with_gh_stderr(self):
        with mock.patch.dict(os.environ, self.ENV):
            with self.assertRaisesRegex(gate_jobs.JobListError, "HTTP 403"):
                gate_jobs.run_jobs(runner=lambda *a, **k: subprocess.CompletedProcess([], 1, "", "HTTP 403"),
                                   sleep=lambda _s: None)


class MainTests(unittest.TestCase):
    """The exit code is the contract: `CI summary` is a required check here.

    Every test above drives a helper. `main()` is what GitHub calls, so a gate
    that returned 0 with a failed job -- or 1 on a clean run -- would either
    stop gating merges or block them forever, and nothing would notice.
    """

    def run_main(self, jobs=None, error=None) -> tuple[int, str]:
        def fake_run_jobs():
            if error is not None:
                raise error
            return jobs or []

        buffer = io.StringIO()
        with (
            mock.patch.object(gate_jobs, "run_jobs", side_effect=fake_run_jobs),
            contextlib.redirect_stdout(buffer),
        ):
            return gate_jobs.main(), buffer.getvalue()

    def test_failing_job_fails_the_gate_and_names_it(self) -> None:
        rc, out = self.run_main([job("linux / Check (ubuntu-latest)", "failure")])
        self.assertEqual(1, rc)
        self.assertIn("::error::linux / Check (ubuntu-latest) did not succeed", out)

    def test_every_green_run_passes_without_annotations(self) -> None:
        rc, out = self.run_main([job("a", "success"), job("b", "skipped")])
        self.assertEqual(0, rc)
        self.assertNotIn("::error::", out)

    def test_unreadable_job_list_fails_closed(self) -> None:
        # Fail closed: today the retry loop normally recovers, but if it ever
        # exhausts, "could not look" must not read as "nothing is wrong".
        rc, out = self.run_main(error=gate_jobs.JobListError("gh said: HTTP 403"))
        self.assertEqual(1, rc)
        self.assertIn("::error::", out)


if __name__ == "__main__":
    unittest.main()

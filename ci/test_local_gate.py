"""A Linux proof must bind both required jobs to the checked-out source."""

import unittest
from dataclasses import replace
from pathlib import Path

from ci.local_gate import (
    JsonValue,
    JobProof,
    RunProof,
    RunSelection,
    load_aware_test_threads,
    verify_run,
)


class RunProofTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path("/repo").resolve()
        self.sha = "a" * 40
        self.proof = RunProof(
            self.root,
            self.sha,
            None,
            "act",
            "0.2.89-act2.2",
            ".github/workflows/check-ubuntu.yml",
            None,
            "minimal",
            "workflow_dispatch",
            "done",
            "success",
            0,
            4,
            4,
            0,
            (
                JobProof("check", "completed", "success"),
                JobProof("python-facade-tests", "completed", "success"),
                JobProof("glibc-floor", "completed", "skipped"),
                JobProof("pool", "completed", "skipped"),
            ),
        )

    def test_complete_source_bound_pair_passes(self) -> None:
        verify_run(self.proof, self.root, self.sha)

    def test_wire_record_is_validated_before_acceptance(self) -> None:
        raw: dict[str, JsonValue] = {
            "workspace": str(self.root),
            "sha": self.sha,
            "dirty": None,
            "engine": "act",
            "act_version": "0.2.89-act2.2",
            "workflow": self.proof.workflow,
            "job": None,
            "mode": "minimal",
            "event": "workflow_dispatch",
            "state": "done",
            "conclusion": "success",
            "exit_code": 0,
            "jobs": {"total": 4, "completed": 4, "failed": 0},
            "tree": {
                "malformed_lines": 0,
                "groups": [
                    {
                        "jobs": [
                            {
                                "job_id": job.job_id,
                                "status": job.status,
                                "conclusion": job.conclusion,
                            }
                            for job in self.proof.jobs
                        ]
                    }
                ],
            },
        }
        verify_run(RunProof.from_json(raw), self.root, self.sha)
        for invalid in (True, -1, "0"):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                RunProof.from_json({**raw, "exit_code": invalid})
        with self.assertRaises(ValueError):
            RunProof.from_json({**raw, "tree": {"malformed_lines": 1, "groups": []}})
        with self.assertRaises(ValueError):
            RunProof.from_json(
                {**raw, "tree": {"malformed_lines": 0, "groups": [None]}}
            )

    def test_missing_ignored_python_suite_fails(self) -> None:
        with self.assertRaises(ValueError):
            verify_run(
                replace(self.proof, jobs=self.proof.jobs[:1]), self.root, self.sha
            )

    def test_advisory_verifier_is_not_part_of_linux_proof(self) -> None:
        verify_run(self.proof, self.root, self.sha)
        with self.assertRaises(ValueError):
            verify_run(
                replace(self.proof, total=4, completed=4,
                        jobs=(*self.proof.jobs, JobProof("verify", "completed", "success"))),
                self.root, self.sha,
            )

    def test_failed_or_skipped_python_suite_fails(self) -> None:
        for verdict in ("failure", "skipped", "cancelled"):
            with self.subTest(verdict=verdict), self.assertRaises(ValueError):
                jobs = (
                    self.proof.jobs[0],
                    replace(self.proof.jobs[1], conclusion=verdict),
                )
                verify_run(replace(self.proof, jobs=jobs), self.root, self.sha)

    def test_unrelated_dirty_or_unfinished_record_fails(self) -> None:
        candidates = (
            replace(self.proof, workspace=Path("/other")),
            replace(self.proof, sha="b" * 40),
            replace(self.proof, dirty=True),
            replace(self.proof, state="running"),
            replace(self.proof, completed=1),
            replace(self.proof, failed=1),
            replace(self.proof, exit_code=1),
            replace(self.proof, act_version="0.2.89"),
            replace(self.proof, workflow=".github/workflows/ci-full.yml"),
            replace(self.proof, job="board_plan"),
        )
        for candidate in candidates:
            with self.subTest(candidate=candidate), self.assertRaises(ValueError):
                verify_run(candidate, self.root, self.sha)

    def test_declared_event_confined_job_must_skip_exactly(self) -> None:
        selection = RunSelection(
            self.proof.workflow,
            None,
            "workflow_dispatch",
            frozenset({"check", "python-facade-tests"}),
            frozenset({"queue-cache-janitor", "glibc-floor", "pool"}),
        )
        skipped = RunProof(
            self.proof.workspace,
            self.proof.sha,
            None,
            self.proof.engine,
            self.proof.act_version,
            self.proof.workflow,
            self.proof.job,
            self.proof.mode,
            self.proof.event,
            "done",
            "success",
            0,
            5,
            5,
            0,
            (
                *self.proof.jobs,
                JobProof("queue-cache-janitor", "completed", "skipped"),
            ),
        )
        verify_run(skipped, self.root, self.sha, selection)
        for candidate in (
            # A declared skip that ran is not the event-confined job.
            replace(skipped, jobs=(*self.proof.jobs, JobProof("queue-cache-janitor", "completed", "success"))),
            # It cannot vanish from the report either.
            replace(skipped, total=2, completed=2, jobs=self.proof.jobs),
        ):
            with self.subTest(candidate=candidate), self.assertRaises(ValueError):
                verify_run(candidate, self.root, self.sha, selection)

    def test_wrong_event_does_not_prove_dispatch_gate(self) -> None:
        with self.assertRaises(ValueError):
            verify_run(replace(self.proof, event="pull_request"), self.root, self.sha)


class LoadAwareTests(unittest.TestCase):
    def test_busy_and_idle_hosts(self) -> None:
        for cpus, load, wanted in ((16, 0, 16), (16, 32, 8), (16, 160, 1), (1, 100, 1)):
            with self.subTest(cpus=cpus, load=load):
                self.assertEqual(wanted, load_aware_test_threads(cpus, load))

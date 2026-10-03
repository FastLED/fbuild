"""A Linux proof must bind both required jobs to the checked-out source."""

import unittest
from dataclasses import replace
from pathlib import Path

from ci.local_gate import JsonValue, JobProof, RunProof, verify_run


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
            ".github/workflows/ci-minimal.yml",
            "linux",
            "minimal",
            "done",
            "success",
            0,
            2,
            2,
            0,
            (
                JobProof("check", "completed", "success"),
                JobProof("python-facade-tests", "completed", "success"),
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
            "job": "linux",
            "mode": "minimal",
            "state": "done",
            "conclusion": "success",
            "exit_code": 0,
            "jobs": {"total": 2, "completed": 2, "failed": 0},
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

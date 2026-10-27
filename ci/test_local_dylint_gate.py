"""A successful Ubuntu proof cannot substitute for ordinary-PR Dylint."""

import copy
import unittest
from pathlib import Path

from ci.local_dylint_gate import REQUIRED_STEPS, SELECTION, verify_dylint
from ci.local_gate import JsonValue


def completed_record() -> dict[str, JsonValue]:
    jobs: list[JsonValue] = []
    for job_id in sorted(SELECTION.required_jobs):
        names = next(
            (required.names for required in REQUIRED_STEPS if required.job == job_id),
            (),
        )
        jobs.append(
            {
                "job_id": job_id,
                "status": "completed",
                "conclusion": "success",
                "sections": [
                    {
                        "name": name,
                        "stage": "Main",
                        "status": "completed",
                        "conclusion": "success",
                    }
                    for name in names
                ],
            }
        )
    return {
        "workspace": "/repo",
        "sha": "a" * 40,
        "dirty": None,
        "engine": "act",
        "act_version": "0.2.89-act2.7",
        "workflow": SELECTION.workflow,
        "job": None,
        "mode": "minimal",
        "event": "pull_request",
        "state": "done",
        "conclusion": "success",
        "exit_code": 0,
        "jobs": {"total": 3, "completed": 3, "failed": 0},
        "tree": {"malformed_lines": 0, "groups": [{"jobs": jobs}]},
    }


class DylintProofTests(unittest.TestCase):
    def test_complete_workflow_and_executed_steps_pass(self) -> None:
        verify_dylint(completed_record(), Path("/repo"), "a" * 40)

    def test_dispatch_full_or_selected_job_cannot_prove_ordinary_pr(self) -> None:
        for change in (
            {"event": "workflow_dispatch"},
            {"mode": "full"},
            {"job": "dylint"},
            {"workflow": ".github/workflows/ci-minimal.yml"},
        ):
            with self.subTest(change=change), self.assertRaises(ValueError):
                verify_dylint({**completed_record(), **change}, Path("/repo"), "a" * 40)

    def test_green_job_with_missing_skipped_or_unfinished_workspace_step_fails(
        self,
    ) -> None:
        for verdict in ("skipped", "failure", None):
            raw = completed_record()
            tree = raw["tree"]
            assert isinstance(tree, dict)
            groups = tree["groups"]
            assert isinstance(groups, list) and isinstance(groups[0], dict)
            jobs = groups[0]["jobs"]
            assert isinstance(jobs, list)
            dylint = next(
                job
                for job in jobs
                if isinstance(job, dict) and job["job_id"] == "dylint"
            )
            assert isinstance(dylint, dict) and isinstance(dylint["sections"], list)
            workspace = dylint["sections"][-1]
            assert isinstance(workspace, dict)
            workspace["conclusion"] = verdict
            with self.subTest(verdict=verdict), self.assertRaises(ValueError):
                verify_dylint(raw, Path("/repo"), "a" * 40)
            dylint["sections"].pop()
            with self.assertRaises(ValueError):
                verify_dylint(raw, Path("/repo"), "a" * 40)

    def test_duplicate_successful_step_is_not_unambiguous_evidence(self) -> None:
        raw = completed_record()
        tree = raw["tree"]
        assert isinstance(tree, dict) and isinstance(tree["groups"], list)
        group = tree["groups"][0]
        assert isinstance(group, dict) and isinstance(group["jobs"], list)
        job = group["jobs"][0]
        assert isinstance(job, dict) and isinstance(job["sections"], list)
        job["sections"].append(copy.deepcopy(job["sections"][0]))
        with self.assertRaises(ValueError):
            verify_dylint(raw, Path("/repo"), "a" * 40)


if __name__ == "__main__":
    unittest.main()

"""The local Dylint lane proves every job of the workflow PRs actually run."""

import copy
import tomllib
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
    for job_id in sorted(SELECTION.skipped_jobs):
        jobs.append(
            {
                "job_id": job_id,
                "status": "completed",
                "conclusion": "skipped",
                "sections": [],
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
        "event": SELECTION.event,
        "state": "done",
        "conclusion": "success",
        "exit_code": 0,
        "jobs": {
            "total": len(SELECTION.required_jobs) + len(SELECTION.skipped_jobs),
            "completed": len(SELECTION.required_jobs) + len(SELECTION.skipped_jobs),
            "failed": 0,
        },
        "tree": {"malformed_lines": 0, "groups": [{"jobs": jobs}]},
    }


class DylintSelectionTests(unittest.TestCase):
    def test_selection_matches_the_declared_replay_selection(self) -> None:
        """The module's SELECTION and local-gate.toml must not drift.

        They are separate declarations of one replay, and drift between them is
        how #2086 happened: the TOML and the module both asked for a
        `pull_request` event the workflow had stopped declaring, so the lane
        failed on every commit while every other test stayed green.
        """
        raw = tomllib.loads(
            (Path(__file__).resolve().parent.parent / "local-gate.toml").read_text()
        )
        selected = [
            item
            for item in raw["gate"]["replay"]["selections"]
            if item["lane"] == "dylint"
        ]
        self.assertEqual(len(selected), 1)
        self.assertEqual(".github/workflows/" + selected[0]["workflow"], SELECTION.workflow)
        self.assertIsNone(SELECTION.job)
        self.assertTrue(selected[0]["all-jobs"])
        self.assertEqual(selected[0]["event"], SELECTION.event)
        self.assertEqual(frozenset({"policy", "dylint", "gate"}), SELECTION.required_jobs)


class DylintProofTests(unittest.TestCase):
    def test_complete_workflow_and_executed_steps_pass(self) -> None:
        verify_dylint(completed_record(), Path("/repo"), "a" * 40)

    def test_full_selected_job_or_another_workflow_cannot_prove_the_jobs(self) -> None:
        # A single selected job, a full-mode run and a caller's entry workflow
        # all execute a different graph than every job of dylint.yml.
        for change in (
            {"mode": "full"},
            {"job": "dylint"},
            {"workflow": ".github/workflows/ci-minimal.yml"},
            {"event": "pull_request"},
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

    def test_event_confined_janitor_must_skip_and_cannot_disappear(self) -> None:
        """dylint.yml's janitor is `merge_group`-only; a dispatch replay must show it skipped."""
        raw = completed_record()
        tree = raw["tree"]
        assert isinstance(tree, dict) and isinstance(tree["groups"], list)
        jobs = tree["groups"][0]["jobs"]
        assert isinstance(jobs, list)
        janitor = next(
            job for job in jobs if isinstance(job, dict) and job["job_id"] == "queue-cache-janitor"
        )
        janitor["conclusion"] = "success"
        with self.assertRaises(ValueError):
            verify_dylint(raw, Path("/repo"), "a" * 40)

        raw = completed_record()
        tree = raw["tree"]
        assert isinstance(tree, dict) and isinstance(tree["groups"], list)
        group = tree["groups"][0]
        assert isinstance(group, dict) and isinstance(group["jobs"], list)
        group["jobs"] = [
            job
            for job in group["jobs"]
            if not (isinstance(job, dict) and job["job_id"] == "queue-cache-janitor")
        ]
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

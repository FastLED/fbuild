"""The actual reusable Ubuntu graph must have complete shared proof coverage."""

import tomllib
import unittest
from pathlib import Path

from ci_lint.workflow_replay_config import parse_replay
from ci_lint.workflow_replay_static import check_replay_static
from ci_lint.workflow_scan import get_on_section, load_workflows

ROOT = Path(__file__).resolve().parent.parent


class SharedReplayTests(unittest.TestCase):
    def test_repository_declares_complete_reusable_ubuntu_graph(self) -> None:
        raw = tomllib.loads((ROOT / "local-gate.toml").read_text())
        self.assertIn("replay", raw["gate"])
        findings = []
        config = parse_replay(
            raw["gate"]["replay"],
            source="local-gate.toml",
            path="gate.replay",
            findings=findings,
        )
        self.assertEqual(findings, [])
        self.assertIsNotNone(config)
        assert config is not None
        self.assertEqual(check_replay_static(config, ROOT), [])
        # Two jobs carry proof. The release cross-link (glibc_floor) is in the
        # graph but provably skipped -- ci-minimal binds a literal false -- so
        # it is declared with no step list and derives no required checks.
        self.assertEqual(
            {
                job.source_job
                for job in config.jobs
                if "linux-minimal" in job.lanes and job.proof.steps
            },
            {
                "check-ubuntu.yml:check",
                "check-ubuntu.yml:python-facade-tests",
            },
        )
        cross_link = [
            job for job in config.jobs
            if job.source_job == "check-ubuntu.yml:glibc-floor"
        ]
        self.assertEqual(len(cross_link), 1)
        self.assertEqual(cross_link[0].proof.steps, ())

    def test_dylint_lane_declares_the_whole_pr_workflow(self):
        raw = tomllib.loads((ROOT / "local-gate.toml").read_text())
        selections = raw["gate"]["replay"]["selections"]
        selected = [item for item in selections if item["lane"] == "dylint"]
        self.assertEqual(len(selected), 1)
        self.assertEqual(selected[0]["workflow"], "dylint.yml")
        self.assertTrue(selected[0]["all-jobs"])
        self.assertNotIn("job", selected[0])
        # `dylint.yml` carries no pull_request trigger since #79a5200a; a
        # selection for an event the file does not declare replays nothing
        # (FastLED/fbuild#2086).
        self.assertEqual(selected[0]["event"], "workflow_dispatch")

    def test_every_selection_event_is_a_trigger_of_its_workflow(self) -> None:
        """A replay selection must name an event the workflow actually declares.

        act2 finds no stage for an undeclared event and the lane dies on every
        commit, so this is the invariant #2086 violated for sixteen days.
        """
        raw = tomllib.loads((ROOT / "local-gate.toml").read_text())
        config = raw["gate"]["replay"]
        files = {item.path: item for item in load_workflows(ROOT)}
        for selection in config["selections"]:
            workflow = selection.get("workflow", raw["gate"]["replay"]["workflow"])
            path = f".github/workflows/{workflow}"
            with self.subTest(lane=selection["lane"], workflow=workflow):
                document = files[path].document
                assert document is not None
                triggers = get_on_section(document)
                self.assertIn(
                    selection["event"],
                    triggers,
                    f"lane {selection['lane']} replays {workflow} with "
                    f"event {selection['event']!r}, which the workflow no longer declares",
                )


if __name__ == "__main__":
    unittest.main()

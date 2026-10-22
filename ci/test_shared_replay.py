"""The actual reusable Ubuntu graph must have complete shared proof coverage."""

import tomllib
import unittest
from pathlib import Path

from ci_lint.workflow_replay_config import parse_replay
from ci_lint.workflow_replay_static import check_replay_static

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
        self.assertEqual(
            {job.source_job for job in config.jobs if "linux-minimal" in job.lanes},
            {
                "ci-minimal.yml:verify",
                "check-ubuntu.yml:check",
                "check-ubuntu.yml:python-facade-tests",
            },
        )

    def test_dylint_lane_declares_the_whole_pr_workflow(self):
        raw = tomllib.loads((ROOT / "local-gate.toml").read_text())
        selections = raw["gate"]["replay"]["selections"]
        selected = [item for item in selections if item["lane"] == "dylint"]
        self.assertEqual(len(selected), 1)
        self.assertEqual(selected[0]["workflow"], "dylint.yml")
        self.assertTrue(selected[0]["all-jobs"])
        self.assertNotIn("job", selected[0])
        self.assertEqual(selected[0]["event"], "pull_request")


if __name__ == "__main__":
    unittest.main()

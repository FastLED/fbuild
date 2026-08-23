from __future__ import annotations

import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from ci import check_workflow_concurrency as guard

CANONICAL = (
    "concurrency:\n"
    "  group: sample.yml-${{ github.event_name == 'pull_request'"
    " && github.ref || github.run_id }}\n"
    "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n"
)


def write(tmp: str, name: str, body: str) -> Path:
    path = Path(tmp) / name
    path.write_text(body, encoding="utf-8")
    return path


class WorkflowConcurrencyTests(unittest.TestCase):
    def test_repo_workflows_all_pass(self) -> None:
        self.assertEqual(guard.main(), 0)

    def test_pr_workflow_without_concurrency_is_rejected(self) -> None:
        with TemporaryDirectory() as tmp:
            path = write(tmp, "sample.yml", "on:\n  pull_request:\n    branches: [main]\njobs: {}\n")

            problems = guard.check_file(path)

        self.assertTrue(any("no top-level" in p for p in problems), problems)

    def test_canonical_block_is_accepted(self) -> None:
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                "on:\n  pull_request:\n    branches: [main]\n\n" + CANONICAL + "jobs: {}\n",
            )

            self.assertEqual(guard.check_file(path), [])

    def test_group_shared_across_non_pr_events_is_rejected(self) -> None:
        """A ref-only group drops queued main SHAs -- only one pending run per group."""
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                "on:\n  push:\n    branches: [main]\n  pull_request:\n"
                "concurrency:\n"
                "  group: sample-${{ github.ref }}\n"
                "  cancel-in-progress: true\n"
                "jobs: {}\n",
            )

            problems = guard.check_file(path)

        self.assertTrue(any("github.run_id" in p for p in problems), problems)

    def test_cancel_disabled_is_rejected(self) -> None:
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                "on:\n  pull_request:\n"
                "concurrency:\n"
                "  group: sample-${{ github.run_id }}\n"
                "  cancel-in-progress: false\n"
                "jobs: {}\n",
            )

            problems = guard.check_file(path)

        self.assertTrue(any("never cancels PR runs" in p for p in problems), problems)

    def test_workflow_without_pr_trigger_is_ignored(self) -> None:
        with TemporaryDirectory() as tmp:
            path = write(tmp, "sample.yml", "on:\n  schedule:\n    - cron: '0 9 * * *'\njobs: {}\n")

            self.assertEqual(guard.check_file(path), [])

    def test_reusable_templates_are_exempt_with_a_reason(self) -> None:
        for name in ("template_build.yml", "template_native_build.yml"):
            self.assertIn("caller", guard.EXEMPT[name])

    def test_every_rendered_board_workflow_carries_the_block(self) -> None:
        boards = sorted(guard.WORKFLOWS.glob("build-*.yml"))

        self.assertGreater(len(boards), 50)
        for path in boards:
            self.assertEqual(guard.check_file(path), [], path.name)


if __name__ == "__main__":
    unittest.main()

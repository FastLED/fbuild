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
            path = write(
                tmp,
                "sample.yml",
                "on:\n  pull_request:\n    branches: [main]\njobs: {}\n",
            )

            problems = guard.check_file(path)

        self.assertTrue(any("no top-level" in p for p in problems), problems)

    def test_canonical_block_is_accepted(self) -> None:
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                "on:\n  pull_request:\n    branches: [main]\n\n"
                + CANONICAL
                + "jobs: {}\n",
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

    def test_a_nightly_sharing_its_group_with_push_runs_is_rejected(self) -> None:
        """The 2026-11-12 nightly was evicted by a merge: only one pending run per group."""
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                "on:\n  push:\n    branches: [main]\n  schedule:\n    - cron: '17 9 * * *'\n"
                "concurrency:\n"
                "  group: sample-${{ github.ref }}\n"
                "  cancel-in-progress: false\n"
                "jobs: {}\n",
            )

            problems = guard.check_file(path)

        self.assertTrue(
            any("scheduled" in p and "pending" in p for p in problems), problems
        )

    def test_a_nightly_with_its_own_group_is_accepted(self) -> None:
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                "on:\n  push:\n    branches: [main]\n  schedule:\n    - cron: '17 9 * * *'\n"
                "concurrency:\n"
                "  group: sample-${{ github.event_name == 'schedule' && 'nightly'"
                " || github.ref }}\n"
                "  cancel-in-progress: false\n"
                "jobs: {}\n",
            )

            self.assertEqual(guard.check_file(path), [])

    def test_a_per_run_group_is_a_safe_scheduled_choice(self) -> None:
        """`github.run_id` is unique per run, so a nightly cannot be evicted."""
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                "on:\n  schedule:\n    - cron: '17 9 * * *'\n"
                "concurrency:\n"
                "  group: sample-${{ github.run_id }}\n"
                "  cancel-in-progress: false\n"
                "jobs: {}\n",
            )

            self.assertEqual(guard.check_file(path), [])

    def test_workflow_without_pr_trigger_is_ignored(self) -> None:
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                "on:\n  schedule:\n    - cron: '0 9 * * *'\njobs: {}\n",
            )

            self.assertEqual(guard.check_file(path), [])

    def test_reusable_templates_are_exempt_with_a_reason(self) -> None:
        for name in ("template_build.yml", "template_native_build.yml"):
            self.assertIn("caller", guard.EXEMPT[name])

    def test_every_rendered_board_workflow_carries_the_block(self) -> None:
        boards = sorted(guard.WORKFLOWS.glob("build-*.yml"))

        self.assertGreater(len(boards), 50)
        for path in boards:
            self.assertEqual(guard.check_file(path), [], path.name)


def deploy_workflow(job_extra: str) -> str:
    """A workflow whose single job deploys GitHub Pages."""
    return (
        "on:\n  push:\n    branches: [main]\n"
        "jobs:\n"
        "  deploy-pages:\n"
        "    runs-on: ubuntu-24.04\n"
        f"{job_extra}"
        "    steps:\n"
        "      - uses: actions/deploy-pages@v4\n"
    )


class PagesDeployConcurrencyTests(unittest.TestCase):
    """Pages allows ONE in-flight deployment per environment.

    `benchmark-build-comparison.yml` now runs two publishers (nightly + push)
    that can overlap on purpose (FastLED/fbuild#2005), and both reach the
    `deploy-pages` job. Without a job-level group the loser's deploy fails with
    "Deployment failed, try again later" (actions/deploy-pages#406), which is
    what GitHub's own Pages template guards against.
    """

    def test_deploy_job_without_concurrency_is_rejected(self) -> None:
        with TemporaryDirectory() as tmp:
            path = write(tmp, "sample.yml", deploy_workflow(""))

            problems = guard.check_file(path)

        self.assertTrue(any("deploy-pages" in p for p in problems), problems)

    def test_serialised_pages_group_is_accepted(self) -> None:
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                deploy_workflow(
                    "    concurrency:\n"
                    "      group: github-pages\n"
                    "      cancel-in-progress: false\n"
                ),
            )

            self.assertEqual(guard.check_file(path), [])

    def test_per_run_pages_group_is_rejected(self) -> None:
        """A per-run group serialises nothing -- every run gets its own slot."""
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                deploy_workflow(
                    "    concurrency:\n"
                    "      group: pages-${{ github.run_id }}\n"
                    "      cancel-in-progress: false\n"
                ),
            )

            problems = guard.check_file(path)

        self.assertTrue(any("deploy-pages" in p for p in problems), problems)

    def test_cancelling_a_pages_deploy_is_rejected(self) -> None:
        """Cancelling mid-deployment can leave the environment stuck."""
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                deploy_workflow(
                    "    concurrency:\n"
                    "      group: github-pages\n"
                    "      cancel-in-progress: true\n"
                ),
            )

            problems = guard.check_file(path)

        self.assertTrue(any("deploy-pages" in p for p in problems), problems)

    def test_uploading_a_pages_artifact_is_not_a_deploy(self) -> None:
        with TemporaryDirectory() as tmp:
            path = write(
                tmp,
                "sample.yml",
                "on:\n  push:\n    branches: [main]\n"
                "jobs:\n"
                "  publish:\n"
                "    runs-on: ubuntu-24.04\n"
                "    steps:\n"
                "      - uses: actions/upload-pages-artifact@v4\n",
            )

            self.assertEqual(guard.check_file(path), [])


if __name__ == "__main__":
    unittest.main()

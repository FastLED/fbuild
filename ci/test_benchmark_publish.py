"""The Pages site must keep publishing when the benchmark is broken (fbuild#1825).

`site/mirror/**` is served to every fbuild client, but it used to reach Pages
only through the benchmark job: `publish` required `needs.benchmark.result ==
'success'` and `deploy-pages` required `publish`. A failing ESP32-S3 benchmark
therefore froze the mirror, even though the mirror has nothing to do with it.

The fix keeps ONE site assembler (Pages deployments replace the whole site, so
two publishers would clobber each other) but makes it tolerant of a missing
measurement: it seeds from the last published `benchmark-stats` branch and only
overlays fresh numbers when the benchmark produced them.
"""

from __future__ import annotations

import unittest
from pathlib import Path

import yaml

WORKFLOW = (
    Path(__file__).resolve().parents[1]
    / ".github"
    / "workflows"
    / "benchmark-build-comparison.yml"
)


def load_workflow() -> dict:
    return yaml.safe_load(WORKFLOW.read_text())


def steps_of(job: dict) -> list[dict]:
    return job["steps"]


def step_named(job: dict, name: str) -> dict:
    matches = [s for s in steps_of(job) if s.get("name") == name]
    assert len(matches) == 1, f"expected exactly one step named {name!r}"
    return matches[0]


def script_of(job: dict, name: str) -> str:
    return step_named(job, name)["run"]


class MirrorDeployIsIndependentTests(unittest.TestCase):
    def setUp(self) -> None:
        self.jobs = load_workflow()["jobs"]

    def test_publish_does_not_require_a_successful_benchmark(self):
        # `always()`/`!cancelled()` keeps the mirror flowing after a failed or
        # timed-out measurement; gating on `result == 'success'` is the bug.
        condition = self.jobs["publish"]["if"]
        self.assertNotIn("needs.benchmark.result == 'success'", condition)
        self.assertIn("!cancelled()", condition)

    def test_benchmark_measurements_are_optional_input(self):
        download = step_named(self.jobs["publish"], "Download benchmark site snapshot")
        self.assertEqual("needs.benchmark.result == 'success'", download["if"])

    def test_assembler_seeds_from_the_last_published_site(self):
        # Without last-good seeding, an always-running publish would replace the
        # whole site with a mirror-only tree and drop the benchmark report.
        script = script_of(
            self.jobs["publish"], "Assemble the site from the last publish plus fresh inputs"
        )
        self.assertIn("refs/heads/benchmark-stats", script)
        self.assertIn("reset --hard FETCH_HEAD", script)
        self.assertLess(
            script.index("refs/heads/benchmark-stats"),
            script.index("cp -a repo/site/mirror/."),
        )

    def test_mirror_is_copied_by_the_assembler_not_the_benchmark(self):
        script = script_of(
            self.jobs["publish"], "Assemble the site from the last publish plus fresh inputs"
        )
        self.assertIn("cp -a repo/site/mirror/.", script)
        benchmark_scripts = " ".join(s.get("run", "") for s in steps_of(self.jobs["benchmark"]))
        self.assertNotIn("repo/site/mirror", benchmark_scripts)

    def test_pages_artifact_uploads_the_assembled_site(self):
        upload = step_named(self.jobs["publish"], "Upload GitHub Pages artifact")
        self.assertEqual("${{ env.publish_dir }}", upload["with"]["path"])

    def test_deploy_only_depends_on_publish(self):
        # `deploy-pages` must not reach past the assembler to the measurement
        # job: its name mentions "benchmark report", so assert on the wiring.
        deploy = self.jobs["deploy-pages"]
        self.assertEqual("publish", deploy["needs"])
        self.assertNotIn("benchmark", deploy["if"])
        self.assertEqual("${{ needs.publish.result == 'success' }}", deploy["if"])

    def test_a_no_op_publish_never_pushes(self):
        # A failing benchmark on an unchanged tree must not rewrite the branch.
        script = script_of(
            self.jobs["publish"], "Assemble the site from the last publish plus fresh inputs"
        )
        self.assertIn("diff --cached --quiet", script)
        self.assertLess(
            script.index("diff --cached --quiet"), script.index("git -C \"${publish_dir}\" push")
        )


if __name__ == "__main__":
    unittest.main()

"""The Pages site must keep publishing when the benchmark is broken (fbuild#1825).

`site/mirror/**` is served to every fbuild client, but it used to reach Pages
only through the benchmark job: `publish` required `needs.benchmark.result ==
'success'` and `deploy-pages` required `publish`. A failing ESP32-S3 benchmark
therefore froze the mirror, even though the mirror has nothing to do with it.

The fix keeps ONE site assembler (Pages deployments replace the whole site, so
two publishers would clobber each other) but makes it tolerant of a missing
measurement: it seeds from the last published `benchmark-stats` branch and only
overlays fresh numbers when the benchmark produced them.

`actions/deploy-pages` also lists the artifacts of the whole RUN, not of the
current ATTEMPT, and refuses to deploy when two are named `github-pages`. Every
re-run of this workflow -- the routine cure for a transient release-asset 504 --
therefore stranded the deploy (runs 42444946413 and 42466790090) and the mirror
manifest with it, so `publish` drops the stale copies before uploading its own.
"""

from __future__ import annotations

import json
import os
import subprocess
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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


class StalePagesArtifactTests(unittest.TestCase):
    """A re-run must not accumulate a second `github-pages` artifact (fbuild#1960)."""

    STEP = "Drop Pages artifacts uploaded by earlier attempts"
    UPLOAD = "Upload GitHub Pages artifact"

    def setUp(self) -> None:
        self.jobs = load_workflow()["jobs"]

    def test_publish_may_delete_run_artifacts(self):
        # DELETE /actions/artifacts/<id> is gated on actions: write.
        self.assertEqual("write", self.jobs["publish"]["permissions"]["actions"])

    def test_stale_artifacts_are_dropped_before_the_upload(self):
        names = [s.get("name") for s in steps_of(self.jobs["publish"])]
        self.assertIn(self.STEP, names)
        self.assertLess(names.index(self.STEP), names.index(self.UPLOAD))

    def test_only_the_pages_artifact_is_deleted(self):
        script = script_of(self.jobs["publish"], self.STEP)
        self.assertIn('select(.name == "github-pages")', script)
        # The benchmark snapshot/log artifacts must survive: they are inputs to
        # the report and the log is the only trace of a failed measurement.
        self.assertNotIn("build-comparison-site", script)
        self.assertNotIn("build-comparison-log", script)

    def _run(self, artifacts: list[dict]) -> tuple[subprocess.CompletedProcess[str], list[tuple[str, str]]]:
        requests: list[tuple[str, str]] = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                requests.append(("GET", self.path))
                body = json.dumps({"total_count": len(artifacts), "artifacts": artifacts}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_DELETE(self) -> None:
                requests.append(("DELETE", self.path))
                self.send_response(204)
                self.end_headers()

            def log_message(self, *args: object) -> None:
                return

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            env = {
                **os.environ,
                "GH_API_URL": f"http://127.0.0.1:{server.server_port}",
                "GH_TOKEN": "test-token",
                "GH_REPO": "FastLED/fbuild",
                "RUN_ID": "42466790090",
            }
            script = script_of(self.jobs["publish"], self.STEP)
            return subprocess.run(
                ["bash", "-c", script], env=env, capture_output=True, text=True, check=False
            ), requests
        finally:
            server.shutdown()
            server.server_close()

    def test_stale_artifacts_are_deleted_and_others_kept(self):
        # Exactly the shape of run 42466790090: two `github-pages` uploads from
        # two attempts plus the benchmark artifacts of the last attempt.
        result, requests = self._run(
            [
                {"id": 11, "name": "github-pages"},
                {"id": 13, "name": "build-comparison-site-deadbeef"},
                {"id": 12, "name": "github-pages"},
                {"id": 14, "name": "build-comparison-log-deadbeef"},
            ]
        )
        self.assertEqual(0, result.returncode, result.stderr)
        deleted = sorted(path for method, path in requests if method == "DELETE")
        self.assertEqual(
            ["/repos/FastLED/fbuild/actions/artifacts/11", "/repos/FastLED/fbuild/actions/artifacts/12"],
            deleted,
        )
        gets = [path for method, path in requests if method == "GET"]
        self.assertEqual(
            ["/repos/FastLED/fbuild/actions/runs/42466790090/artifacts?per_page=100"], gets
        )

    def test_a_first_attempt_deletes_nothing(self):
        result, requests = self._run(
            [{"id": 13, "name": "build-comparison-site-deadbeef"}]
        )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual([], [path for method, path in requests if method == "DELETE"])
        self.assertIn("No github-pages artifact", result.stdout)


if __name__ == "__main__":
    unittest.main()

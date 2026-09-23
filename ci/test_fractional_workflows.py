"""Contract checks for the generated fractional CI entry points."""

import unittest
import os
import subprocess
import tempfile

import yaml

from ci import render_workflows


class FractionalWorkflowTests(unittest.TestCase):
    def load(self, name):
        # PyYAML treats `on` as a YAML 1.1 boolean; inspect its True key.
        return yaml.safe_load((render_workflows.WORKFLOWS_DIR / name).read_text())

    def test_board_workflows_have_no_independent_pr_or_main_trigger(self):
        for board in render_workflows.load_sot()["boards"]:
            with self.subTest(board=board["workflow"]):
                events = self.load(board["workflow"])[True]
                self.assertNotIn("push", events)
                self.assertNotIn("pull_request", events)
                self.assertIn("workflow_call", events)

    def test_full_matrix_matches_every_supported_board(self):
        boards = render_workflows.load_sot()["boards"]
        full = self.load("ci-full.yml")
        self.assertNotIn("paths", full[True].get("pull_request", {}))
        matrix = full["jobs"]["boards"]["strategy"]["matrix"]["include"]
        self.assertEqual({b["workflow"] for b in boards}, {alias for b in matrix for alias in b["workflow_aliases"]})
        self.assertEqual(len(render_workflows.execution_boards(boards)), len(matrix))
        self.assertEqual(len(matrix), len({(b["test_dir"], b["env_name"], b["firmware_ext"]) for b in matrix}))
        self.assertFalse(full["jobs"]["boards"]["strategy"]["fail-fast"])
        self.assertEqual("./.github/workflows/check-ubuntu.yml", full["jobs"]["linux"]["uses"])
        self.assertEqual("./.github/workflows/check-windows.yml", full["jobs"]["windows"]["uses"])
        self.assertEqual("./.github/workflows/dylint.yml", full["jobs"]["dylint"]["uses"])
        self.assertTrue(full["jobs"]["dylint"]["with"]["run_full"])
        dylint = self.load("dylint.yml")
        self.assertEqual("Dylint", dylint["jobs"]["policy"]["name"])
        self.assertIn("inputs.run_full", dylint["jobs"]["dylint"]["if"])
        for job, workflow in (
            ("acceptance", "acceptance-205.yml"),
            ("bench", "bench-205.yml"),
            ("qemu", "qemu-linux-runtime.yml"),
        ):
            self.assertEqual(f"./.github/workflows/{workflow}", full["jobs"][job]["uses"])
            self.assertEqual("${{ needs.verify.outputs.candidate_sha }}", full["jobs"][job]["with"]["ref"])
            self.assertNotIn("pull_request", self.load(workflow)[True])
        self.assertEqual("${{ needs.verify.outputs.candidate_sha }}", full["jobs"]["boards"]["with"]["checkout_ref"])
        for job, workflow_name in (
            ("fmt", "fmt.yml"), ("docs", "docs.yml"), ("msrv", "msrv.yml"),
            ("validate_boards", "validate-boards.yml"), ("crate_gate", "crate-gate.yml"),
        ):
            self.assertEqual(f"./.github/workflows/{workflow_name}", full["jobs"][job]["uses"])
            self.assertEqual("${{ needs.verify.outputs.candidate_sha }}", full["jobs"][job]["with"]["ref"])
            self.assertIn("workflow_call", self.load(workflow_name)[True])
            self.assertEqual("${{ inputs.ref }}", self.load(workflow_name)["jobs"][next(iter(self.load(workflow_name)["jobs"]))]["steps"][0]["with"]["ref"])
            self.assertIn("inputs.ref != ''", self.load(workflow_name)["concurrency"]["group"])

    def test_fast_matrix_matches_selected_boards(self):
        selected = [b["workflow"] for b in render_workflows.load_sot()["boards"] if b.get("fractional")]
        fast = self.load("ci-test.yml")
        matrix = fast["jobs"]["boards"]["strategy"]["matrix"]["include"]
        self.assertEqual(selected, [b["workflow"] for b in matrix])
        self.assertEqual(1, len(matrix))
        full_matrix = self.load("ci-full.yml")["jobs"]["boards"]["strategy"]["matrix"]["include"]
        for cell in matrix:
            self.assertIn(cell, full_matrix)

    def test_nightly_deduplicates_identical_builds_but_keeps_aliases(self):
        boards = render_workflows.load_sot()["boards"]
        nightly = self.load("nightly-platforms.yml")
        matrix = nightly["jobs"]["build"]["strategy"]["matrix"]["include"]
        self.assertEqual(len(render_workflows.execution_boards(boards)), len(matrix))
        self.assertEqual({b["workflow"] for b in boards}, {alias for b in matrix for alias in b["workflow_aliases"]})

    def test_ordinary_minimal_and_opt_in_test_are_distinct(self):
        minimal = self.load("ci-minimal.yml")
        fast = self.load("ci-test.yml")
        self.assertIn("push", minimal[True])
        self.assertIn("pull_request", minimal[True])
        self.assertEqual({"linux", "test", "full", "selected-coverage"}, set(minimal["jobs"]))
        self.assertIn("github.event.pull_request.head.sha", minimal["jobs"]["linux"]["with"]["ref"])
        self.assertIn("ci-test", minimal["jobs"]["linux"]["if"])
        self.assertIn("ci-full", minimal["jobs"]["linux"]["if"])
        self.assertNotIn("push", fast[True])
        self.assertIn("workflow_call", fast[True])
        self.assertIn("ci-test", fast["jobs"]["boards"]["if"])

    def test_labeled_tiers_follow_head_sha_and_invalidate_removed_labels(self):
        minimal = self.load("ci-minimal.yml")
        self.assertEqual(
            ["opened", "labeled", "unlabeled", "synchronize", "reopened"],
            minimal[True]["pull_request"]["types"],
        )
        for tier in ("test", "full"):
            with self.subTest(tier=tier):
                workflow = self.load(f"ci-{tier}.yml")
                self.assertNotIn("pull_request", workflow[True])
                selected = minimal["jobs"][tier]
                self.assertIn(f"'ci-{tier}'", selected["if"])
                self.assertEqual(f"./.github/workflows/ci-{tier}.yml", selected["uses"])
                self.assertEqual("${{ github.event.pull_request.head.sha }}", selected["with"]["candidate_sha"])
                if tier == "test":
                    self.assertIn("!contains(github.event.pull_request.labels.*.name, 'ci-full')", selected["if"])
                self.assertTrue(workflow[True]["workflow_dispatch"]["inputs"]["candidate_sha"]["required"])
                self.assertTrue(workflow[True]["workflow_call"]["inputs"]["candidate_sha"]["required"])
                self.assertIn("github.event.pull_request.head.sha", workflow["jobs"]["verify"]["steps"][0]["with"]["ref"])
                verify_step = workflow["jobs"]["verify"]["steps"][1]
                self.assertEqual("${{ github.sha }}", verify_step["env"]["WORKFLOW_SHA"])
                self.assertIn('"$CANDIDATE_SHA" != "$WORKFLOW_SHA"', verify_step["run"])
                self.assertIn("always()", workflow["jobs"]["coverage"]["if"])
                self.assertIn("complete=false", workflow["jobs"]["coverage"]["steps"][0]["run"])
                self.assertNotIn("invalidate", workflow["jobs"])
                self.assertEqual("verify", workflow["jobs"]["boards"]["needs"])
                self.assertEqual("${{ needs.verify.outputs.candidate_sha }}", workflow["jobs"]["boards"]["with"]["checkout_ref"])

    def test_selected_coverage_requires_every_requested_tier(self):
        minimal = self.load("ci-minimal.yml")
        job = minimal["jobs"]["selected-coverage"]
        self.assertEqual({"linux", "test", "full"}, set(job["needs"]))
        self.assertIn("always()", job["if"])
        script = job["steps"][0]["run"]
        base = {**os.environ, "LINUX": "success", "TEST_SELECTED": "false", "TEST_RESULT": "skipped", "FULL_SELECTED": "false", "FULL_RESULT": "skipped", "FULL_COVERAGE": ""}
        cases = [
            ({}, 0),
            ({"TEST_SELECTED": "true", "TEST_RESULT": "skipped"}, 1),
            ({"TEST_SELECTED": "true", "TEST_RESULT": "success", "LINUX": "skipped"}, 0),
            ({"FULL_SELECTED": "true", "FULL_RESULT": "skipped"}, 1),
            ({"FULL_SELECTED": "true", "FULL_RESULT": "success", "FULL_COVERAGE": "false"}, 1),
            ({"FULL_SELECTED": "true", "FULL_RESULT": "success", "FULL_COVERAGE": "true", "LINUX": "skipped"}, 0),
            ({"TEST_SELECTED": "true", "TEST_RESULT": "skipped", "FULL_SELECTED": "true", "FULL_RESULT": "success", "FULL_COVERAGE": "true", "LINUX": "skipped"}, 0),
            ({"LINUX": "failure"}, 1),
        ]
        for overrides, expected in cases:
            with self.subTest(overrides=overrides):
                result = subprocess.run(["bash", "-e", "-c", script], env={**base, **overrides}, capture_output=True, text=True)
                self.assertEqual(expected, result.returncode)

    def test_full_coverage_sentinel_and_release_gate(self):
        full = self.load("ci-full.yml")
        self.assertEqual(
            {"verify", "boards", "linux", "windows", "dylint", "acceptance", "bench", "qemu", "fmt", "docs", "msrv", "validate_boards", "crate_gate"},
            set(full["jobs"]["coverage"]["needs"]),
        )
        self.assertEqual("${{ jobs.coverage.outputs.complete }}", full[True]["workflow_call"]["outputs"]["coverage"]["value"])
        script = full["jobs"]["coverage"]["steps"][0]["run"]
        env = {**os.environ, "EVENT_NAME": "workflow_call", "LABEL_PRESENT": "false"}
        env.update({key: "success" for key in ("VERIFY", "BOARDS", "LINUX", "WINDOWS", "DYLINT", "ACCEPTANCE", "BENCH", "QEMU", "FMT", "DOCS", "MSRV", "VALIDATE_BOARDS", "CRATE_GATE")})
        with tempfile.NamedTemporaryFile() as output:
            env["GITHUB_OUTPUT"] = output.name
            for missing in ("FMT", "DOCS", "MSRV", "VALIDATE_BOARDS", "CRATE_GATE"):
                with self.subTest(missing=missing):
                    result = subprocess.run(["bash", "-e", "-c", script], env={**env, missing: "skipped"}, capture_output=True, text=True)
                    self.assertNotEqual(0, result.returncode)
            self.assertNotIn("complete=true", output.read().decode())
        release = self.load("release-auto.yml")
        self.assertEqual("${{ needs.prepare.outputs.release_ref }}", release["jobs"]["full-ci"]["with"]["candidate_sha"])
        for job in ("publish", "publish-pypi"):
            self.assertIn("needs.full-ci.outputs.coverage == 'true'", release["jobs"][job]["if"])

    def test_release_publication_waits_for_full_validation(self):
        release = self.load("release-auto.yml")
        full = release["jobs"]["full-ci"]
        self.assertEqual("./.github/workflows/ci-full.yml", full["uses"])
        self.assertEqual("${{ needs.prepare.outputs.release_ref }}", full["with"]["candidate_sha"])
        self.assertEqual("${{ needs.prepare.outputs.release_ref }}", release["jobs"]["build"]["with"]["ref"])
        prepare_script = next(step["run"] for step in release["jobs"]["prepare"]["steps"] if step.get("id") == "prepare")
        self.assertIn("git rev-parse 'FETCH_HEAD^{commit}'", prepare_script)
        self.assertIn("[[ ! \"$release_ref\" =~ ^[0-9a-f]{40}$ ]]", prepare_script)
        self.assertIn("full-ci", release["jobs"]["publish"]["needs"])
        self.assertIn("full-ci", release["jobs"]["publish-pypi"]["needs"])

    def test_release_requires_explicit_exact_candidate(self):
        release = self.load("release-auto.yml")
        events = release[True]
        self.assertNotIn("push", events)
        self.assertNotIn("pull_request", events)
        self.assertTrue(events["workflow_dispatch"]["inputs"]["candidate_sha"]["required"])
        self.assertEqual(
            "${{ inputs.candidate_sha }}",
            release["jobs"]["prepare"]["steps"][0]["with"]["ref"],
        )
        prepare_script = next(step["run"] for step in release["jobs"]["prepare"]["steps"] if step.get("id") == "prepare")
        self.assertIn("[[ ! \"$CANDIDATE_SHA\" =~ ^[0-9a-f]{40}$ ]]", prepare_script)
        self.assertIn('"$CANDIDATE_SHA" != "$WORKFLOW_SHA"', prepare_script)
        prepare_step = next(step for step in release["jobs"]["prepare"]["steps"] if step.get("id") == "prepare")
        self.assertEqual("${{ github.sha }}", prepare_step["env"]["WORKFLOW_SHA"])
        self.assertIn('if [ "$commit_sha" != "$CANDIDATE_SHA" ]', prepare_script)
        self.assertNotIn('if [ "${GITHUB_EVENT_NAME}" != "workflow_dispatch" ]', prepare_script)

    def test_publication_waits_for_trusted_runtime_coverage(self):
        release = self.load("release-auto.yml")
        runtime = release["jobs"]["runtime-coverage"]
        self.assertIn("full-ci", runtime["needs"])
        self.assertIn("publish", runtime["if"])
        self.assertEqual("prepare", release["jobs"]["build"]["needs"])
        self.assertEqual("prepare", release["jobs"]["full-ci"]["needs"])
        guard_script = runtime["steps"][0]["run"]
        self.assertIn("physical-board runtime coverage", guard_script)
        self.assertIn("exit 1", guard_script)
        self.assertNotIn("runtime_coverage", release[True]["workflow_dispatch"]["inputs"])
        for job in ("publish", "build-pypi", "publish-pypi"):
            self.assertIn("runtime-coverage", release["jobs"][job]["needs"])


if __name__ == "__main__":
    unittest.main()

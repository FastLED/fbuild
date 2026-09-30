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
        self.assertEqual("./.github/workflows/check-macos.yml", full["jobs"]["macos"]["uses"])
        self.assertNotIn("macos", self.load("ci-minimal.yml")["jobs"])
        self.assertNotIn("macos", self.load("ci-test.yml")["jobs"])
        macos = self.load("check-macos.yml")
        self.assertEqual(
            {"macos-15-intel", "macos-15"},
            set(macos["jobs"]["test"]["strategy"]["matrix"]["runner"]),
        )
        self.assertEqual("./.github/workflows/dylint.yml", full["jobs"]["dylint"]["uses"])
        self.assertNotIn("run_full", full["jobs"]["dylint"]["with"])
        dylint = self.load("dylint.yml")
        self.assertEqual(["main"], dylint[True]["push"]["branches"])
        setup = next(
            step for step in dylint["jobs"]["dylint"]["steps"]
            if step.get("uses", "").startswith("zackees/setup-soldr@")
        )
        self.assertEqual("dylint-unified-v2", setup["with"]["cache-key-suffix"])
        self.assertEqual("Dylint policy", dylint["jobs"]["policy"]["name"])
        self.assertNotIn("if", dylint["jobs"]["dylint"])
        lint_job = dylint["jobs"]["dylint"]
        self.assertEqual("ubuntu-latest", lint_job["runs-on"])
        self.assertNotIn("strategy", lint_job)
        lint_run = next(
            step["run"]
            for step in lint_job["steps"]
            if step.get("name") == "Run dylint over workspace"
        )
        for triple in (
            "x86_64-unknown-linux-gnu",
            "x86_64-pc-windows-msvc",
            "x86_64-apple-darwin",
        ):
            self.assertIn(triple, lint_run)
        self.assertIn("inputs.ref != ''", next(step for step in lint_job["steps"] if step.get("name") == "Run dylint over workspace")["env"]["FULL_TARGETS"])
        # Soldr provisions the cross-target rust-std itself (zackees/soldr#3426);
        # the temporary explicit `rustup target add` bridge must stay gone (#1527).
        self.assertNotIn(
            "rustup target add",
            "\n".join(step.get("run", "") for step in lint_job["steps"]),
        )
        setup = next(step for step in lint_job["steps"] if step.get("uses", "").startswith("zackees/setup-soldr@"))
        self.assertIs(setup["with"]["dylint-output-cache"], False)
        restore = next(step for step in lint_job["steps"] if step.get("name") == "Restore compiled Dylint libraries")
        self.assertIn("hashFiles('dylints/**')", restore["with"]["key"])
        self.assertTrue(any(step.get("name") == "Save compiled Dylint libraries" for step in lint_job["steps"]))
        ui_tests = next(step for step in lint_job["steps"] if step.get("name") == "Test Dylint libraries")
        self.assertIn("git diff --quiet FETCH_HEAD HEAD -- dylints", ui_tests["run"])
        gate = dylint["jobs"]["gate"]
        self.assertEqual("Dylint", gate["name"])
        self.assertEqual({"policy", "dylint"}, set(gate["needs"]))
        self.assertIn("always()", gate["if"])
        self.assertIn("needs.dylint.result", gate["steps"][0]["env"]["FULL_DYLINT"])
        gate_script = gate["steps"][0]["run"]
        for policy_result, dylint_result, expected in (
            ("success", "success", 0),
            ("failure", "success", 1),
            ("success", "failure", 1),
            ("success", "skipped", 1),
        ):
            with self.subTest(policy=policy_result, dylint=dylint_result):
                result = subprocess.run(
                    ["bash", "-e", "-c", gate_script],
                    env={**os.environ, "POLICY": policy_result, "FULL_DYLINT": dylint_result},
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(expected, result.returncode)
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
            ("fmt", "fmt.yml"), 
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

    def test_nightly_dispatches_board_workflows_as_their_own_runs(self):
        # Badges track runs of each build-<board>.yml file, so the nightly /
        # push-to-main path must dispatch them (not build in a matrix).
        nightly = self.load("nightly-platforms.yml")
        self.assertEqual({"plan", "fbuild_bin", "dispatch"}, set(nightly["jobs"]))
        self.assertEqual(["main"], nightly[True]["push"]["branches"])
        self.assertIn("schedule", nightly[True])
        self.assertIn("select_boards.py", nightly["jobs"]["plan"]["steps"][2]["run"])
        self.assertEqual("write", nightly["jobs"]["dispatch"]["permissions"]["actions"])
        run = nightly["jobs"]["dispatch"]["steps"][0]["run"]
        self.assertIn("gh workflow run", run)
        self.assertIn('fbuild-run-id="$GITHUB_RUN_ID"', run)

    def test_board_workflows_reuse_a_dispatched_fbuild_binary(self):
        for board in render_workflows.load_sot()["boards"]:
            with self.subTest(workflow=board["workflow"]):
                wf = self.load(board["workflow"])
                self.assertIn("fbuild-run-id", wf[True]["workflow_dispatch"]["inputs"])
                with_ = wf["jobs"]["build"]["with"]
                self.assertEqual("${{ inputs.fbuild-run-id }}", with_["fbuild-run-id"])
                self.assertEqual(board["test_dir"], with_["test-dir"])
                self.assertEqual(
                    render_workflows.toolchain_cache(board, render_workflows.load_sot()["families"]),
                    with_["toolchain-cache"],
                )

    def test_toolchain_cache_is_opt_in_with_a_recorded_measurement(self):
        for name, family in render_workflows.load_sot()["families"].items():
            with self.subTest(family=name):
                if family.get("toolchain_cache"):
                    self.assertIn("_toolchain_cache_why", family)

    def test_ordinary_minimal_and_opt_in_test_are_distinct(self):
        minimal = self.load("ci-minimal.yml")
        fast = self.load("ci-test.yml")
        self.assertIn("push", minimal[True])
        self.assertIn("pull_request", minimal[True])
        self.assertEqual(
            {"linux", "board_plan", "fbuild_bin", "pr_boards", "reuse_decision", "test", "full", "selected-coverage"},
            set(minimal["jobs"]),
        )
        # GEN-021 shadow decision (zackees/ci.yml#162): main pushes only, never
        # consumed by another job, and unable to fail the run.
        reuse = minimal["jobs"]["reuse_decision"]
        self.assertIn("github.event_name == 'push'", reuse["if"])
        self.assertTrue(reuse.get("continue-on-error"))
        self.assertTrue(all(step.get("continue-on-error") for step in reuse["steps"]))
        # A stall must end as a tolerated step timeout, never a job cancellation.
        step_budget = sum(step["timeout-minutes"] for step in reuse["steps"])
        self.assertLess(step_budget, reuse["timeout-minutes"])
        self.assertIn("--mode shadow", reuse["steps"][-1]["run"])
        self.assertFalse(any("reuse_decision" in str(job.get("needs", "")) for job in minimal["jobs"].values()))
        # Path-selected boards run only on the default PR tier.
        plan_if = minimal["jobs"]["board_plan"]["if"]
        self.assertIn("pull_request", plan_if)
        self.assertIn("ci-test", plan_if)
        self.assertIn("ci-full", plan_if)
        self.assertIn("--matrix", minimal["jobs"]["board_plan"]["steps"][-1]["run"])
        self.assertEqual("fbuild-bin-linux-debug", minimal["jobs"]["pr_boards"]["with"]["fbuild-artifact"])
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
                self.assertEqual(["verify", "fbuild_bin"], workflow["jobs"]["boards"]["needs"])
                self.assertEqual("fbuild-bin-linux-debug", workflow["jobs"]["boards"]["with"]["fbuild-artifact"])
                self.assertEqual(
                    "${{ needs.verify.outputs.candidate_sha }}",
                    workflow["jobs"]["fbuild_bin"]["steps"][0]["with"]["ref"],
                )
                self.assertEqual("${{ needs.verify.outputs.candidate_sha }}", workflow["jobs"]["boards"]["with"]["checkout_ref"])

    def test_selected_coverage_requires_every_requested_tier(self):
        minimal = self.load("ci-minimal.yml")
        job = minimal["jobs"]["selected-coverage"]
        self.assertEqual({"linux", "test", "full", "pr_boards"}, set(job["needs"]))
        self.assertIn("always()", job["if"])
        script = job["steps"][0]["run"]
        base = {**os.environ, "LINUX": "success", "TEST_SELECTED": "false", "TEST_RESULT": "skipped", "FULL_SELECTED": "false", "FULL_RESULT": "skipped", "FULL_COVERAGE": "", "PR_BOARDS": "skipped"}
        cases = [
            ({}, 0),
            ({"TEST_SELECTED": "true", "TEST_RESULT": "skipped"}, 1),
            ({"TEST_SELECTED": "true", "TEST_RESULT": "success", "LINUX": "skipped"}, 0),
            ({"FULL_SELECTED": "true", "FULL_RESULT": "skipped"}, 1),
            ({"FULL_SELECTED": "true", "FULL_RESULT": "success", "FULL_COVERAGE": "false"}, 1),
            ({"FULL_SELECTED": "true", "FULL_RESULT": "success", "FULL_COVERAGE": "true", "LINUX": "skipped"}, 0),
            ({"TEST_SELECTED": "true", "TEST_RESULT": "skipped", "FULL_SELECTED": "true", "FULL_RESULT": "success", "FULL_COVERAGE": "true", "LINUX": "skipped"}, 0),
            ({"LINUX": "failure"}, 1),
            ({"PR_BOARDS": "success"}, 0),
            ({"PR_BOARDS": "failure"}, 1),
            ({"PR_BOARDS": "cancelled"}, 1),
        ]
        for overrides, expected in cases:
            with self.subTest(overrides=overrides):
                result = subprocess.run(["bash", "-e", "-c", script], env={**base, **overrides}, capture_output=True, text=True)
                self.assertEqual(expected, result.returncode)

    def test_full_coverage_sentinel_and_release_gate(self):
        full = self.load("ci-full.yml")
        self.assertEqual(
            {"verify", "boards", "linux", "windows", "macos", "dylint", "acceptance", "bench", "qemu", "fmt", "validate_boards", "crate_gate"},
            set(full["jobs"]["coverage"]["needs"]),
        )
        self.assertEqual("${{ jobs.coverage.outputs.complete }}", full[True]["workflow_call"]["outputs"]["coverage"]["value"])
        script = full["jobs"]["coverage"]["steps"][0]["run"]
        env = {**os.environ, "EVENT_NAME": "workflow_call", "LABEL_PRESENT": "false"}
        env.update({key: "success" for key in ("VERIFY", "BOARDS", "LINUX", "WINDOWS", "MACOS", "DYLINT", "ACCEPTANCE", "BENCH", "QEMU", "FMT", "VALIDATE_BOARDS", "CRATE_GATE")})
        with tempfile.NamedTemporaryFile() as output:
            env["GITHUB_OUTPUT"] = output.name
            for missing in ("FMT", "VALIDATE_BOARDS", "CRATE_GATE"):
                with self.subTest(missing=missing):
                    result = subprocess.run(["bash", "-e", "-c", script], env={**env, missing: "skipped"}, capture_output=True, text=True)
                    self.assertNotEqual(0, result.returncode)
            self.assertNotIn("complete=true", output.read().decode())
        release = self.load("release-auto.yml")
        self.assertEqual("${{ needs.prepare.outputs.release_ref }}", release["jobs"]["full-ci"]["with"]["candidate_sha"])
        self.assertEqual(
            {"macos-15-intel", "macos-15"},
            {cell["runner"] for cell in release["jobs"]["macos-release-smoke"]["strategy"]["matrix"]["include"]},
        )
        self.assertIn("macos-release-smoke", release["jobs"]["publish"]["needs"])
        self.assertIn("macos-release-smoke", release["jobs"]["build-pypi"]["needs"])
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

    def test_publication_uses_software_validation_without_hardware_gate(self):
        release = self.load("release-auto.yml")
        self.assertNotIn("runtime-coverage", release["jobs"])
        self.assertEqual({"contents": "read"}, release["permissions"])
        self.assertEqual("prepare", release["jobs"]["build"]["needs"])
        self.assertEqual("prepare", release["jobs"]["full-ci"]["needs"])
        self.assertNotIn("runtime_coverage", release[True]["workflow_dispatch"]["inputs"])
        for job in ("publish", "build-pypi", "publish-pypi"):
            self.assertIn("full-ci", release["jobs"][job]["needs"])
            self.assertIn("needs.full-ci.outputs.coverage == 'true'", release["jobs"][job]["if"])
            self.assertNotIn("runtime-coverage", release["jobs"][job]["needs"])
        self.assertEqual(
            {"contents": "write", "attestations": "write", "id-token": "write"},
            release["jobs"]["publish"]["permissions"],
        )
        self.assertEqual(
            {"contents": "read", "attestations": "write", "id-token": "write"},
            release["jobs"]["build"]["permissions"],
        )
        self.assertEqual(
            {"contents": "read", "id-token": "write"},
            release["jobs"]["publish-pypi"]["permissions"],
        )


if __name__ == "__main__":
    unittest.main()

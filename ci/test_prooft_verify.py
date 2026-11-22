"""Contract tests for ci-minimal's `prooft` job, `./push` and the runner lanes.

    uv run --with pyyaml --no-project python -m unittest ci.test_prooft_verify

The verifier may only ever *cost CI time*: every path that cannot prove a
skip (wrong event, fork, fetch failure, verifier crash) must emit
``skip_<job>=false`` for every job ci-minimal reads, and a pull_request must
be judged by the pinned prooft revision against the PR base.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import os
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

import yaml

from ci import local_gate, prooft_verify

ROOT = Path(__file__).resolve().parents[1]
LINT_CHECKS = {"fmt", "lint", "clippy", "dylint", "types", "check", "static"}
PR_ENV = {"EVENT_NAME": "pull_request", "BASE_SHA": "b" * 40, "HEAD_SHA": "a" * 40, "PR_TITLE": "t", "FORK": "false"}


def load_push():
    loader = importlib.machinery.SourceFileLoader("fbuild_push", str(ROOT / "push"))
    spec = importlib.util.spec_from_loader("fbuild_push", loader)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class VerifierTests(unittest.TestCase):
    def run_main(self, env: dict[str, str], returncodes: list[int]) -> tuple[int, list[list[str]], str]:
        calls: list[list[str]] = []
        codes = list(returncodes)

        def fake_run(argv, **_kwargs):
            calls.append(list(argv))
            return subprocess.CompletedProcess(argv, codes.pop(0) if codes else 0)

        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            with (
                patch.dict(os.environ, {**env, "GITHUB_OUTPUT": str(out)}, clear=True),
                patch.object(prooft_verify.subprocess, "run", fake_run),
            ):
                code = prooft_verify.main()
            written = out.read_text() if out.exists() else ""
        return code, calls, written

    def assert_runs_everything(self, written: str) -> None:
        for job in prooft_verify.JOBS:
            self.assertIn(f"skip_{job}=false\n", written)

    def test_non_pr_events_run_everything_without_calling_prooft(self) -> None:
        for event in ("push", "workflow_dispatch"):
            code, calls, written = self.run_main({**PR_ENV, "EVENT_NAME": event}, [])
            self.assertEqual(0, code)
            self.assertEqual([], calls)
            self.assert_runs_everything(written)

    def test_fork_prs_run_everything(self) -> None:
        code, calls, written = self.run_main({**PR_ENV, "FORK": "true"}, [])
        self.assertEqual((0, []), (code, calls))
        self.assert_runs_everything(written)

    def test_pr_is_planned_by_the_pinned_prooft_against_the_base(self) -> None:
        code, calls, _ = self.run_main(PR_ENV, [0, 0])
        self.assertEqual(0, code)
        self.assertEqual(["git", "fetch", "-q", "origin", "a" * 40], calls[0])
        plan = calls[1]
        self.assertEqual(["uvx", "--from", prooft_verify.PROOFT_SOURCE, "prooft"], plan[:4])
        self.assertRegex(prooft_verify.PROOFT_SOURCE, r"@[0-9a-f]{40}$")
        for flag, value in (("--base", "b" * 40), ("--head", "a" * 40), ("--title", "t")):
            self.assertEqual(value, plan[plan.index(flag) + 1])
        self.assertIn("--github-output", plan)

    def test_merge_group_plans_actual_pr_and_queue_heads(self) -> None:
        base, queued, pr = "b" * 40, "a" * 40, "c" * 40
        env = {
            **PR_ENV,
            "EVENT_NAME": "merge_group",
            "QUEUE_REF": "refs/heads/gh-readonly-queue/main/pr-42-" + base,
            "GITHUB_REPOSITORY": "FastLED/fbuild",
        }

        def api(argv, **_kwargs):
            self.assertEqual(["gh", "api", "repos/FastLED/fbuild/pulls/42"], argv[:3])
            return subprocess.CompletedProcess(argv, 0, f'["{pr}", "FastLED/fbuild", "feature", "open", false]')

        plan = subprocess.CompletedProcess([], 0)
        history = subprocess.CompletedProcess([], 0, base)
        with (
            patch.dict(os.environ, env, clear=True),
            patch.object(prooft_verify, "fetch", return_value=True) as fetch,
            patch.object(prooft_verify.subprocess, "run", side_effect=[history, plan]) as run,
            patch.object(prooft_verify, "queue_pr_head", return_value=pr) as lookup,
        ):
            self.assertEqual(0, prooft_verify.main())
        lookup.assert_called_once_with(env["QUEUE_REF"], env["GITHUB_REPOSITORY"])
        self.assertEqual([pr, queued], [call.args[0] for call in fetch.call_args_list])
        self.assertEqual(
            ["queue", "--base", base, "--pr-head", pr, "--queue-head", queued, "--github-output"],
            run.call_args.args[0][6:],
        )
        with patch.object(prooft_verify.subprocess, "run", api):
            self.assertEqual(pr, prooft_verify.queue_pr_head(env["QUEUE_REF"], env["GITHUB_REPOSITORY"]))

    def test_merge_method_credits_only_verified_identical_trees(self) -> None:
        base, queued, pr, other = "b" * 40, "a" * 40, "c" * 40, "d" * 40
        tree, changed = "e" * 40, "f" * 40
        cases = (
            ([base, pr], [tree, tree], "plan"),
            ([base, pr], [tree, changed], "queue"),
            ([other, pr], [], "queue"),
            ([base, other], [], None),
        )
        for parents, trees, command in cases:
            with (
                self.subTest(parents=parents, trees=trees),
                patch.object(prooft_verify, "queue_pr_head", return_value=pr),
                patch.object(prooft_verify, "fetch", return_value=True),
                patch.object(prooft_verify, "plan", return_value=0) as plan,
                patch.object(prooft_verify, "run_all", return_value=0) as run_all,
                patch.object(
                    prooft_verify.subprocess, "run",
                    side_effect=[subprocess.CompletedProcess([], 0, " ".join(values))
                                 for values in (parents, trees)],
                ) as git,
            ):
                self.assertEqual(0, prooft_verify.queue(base, queued))
            if command == "plan":
                plan.assert_called_once_with("plan", "--base", base, "--head", pr)
            elif command == "queue":
                plan.assert_called_once_with(
                    "queue", "--base", base, "--pr-head", pr, "--queue-head", queued,
                )
            else:
                plan.assert_not_called()
                run_all.assert_called_once()
            if trees:
                self.assertEqual(
                    ["git", "rev-parse", f"{pr}^{{tree}}", f"{queued}^{{tree}}"],
                    git.call_args.args[0],
                )

    def test_invalid_queue_metadata_runs_everything(self) -> None:
        code, calls, written = self.run_main({**PR_ENV, "EVENT_NAME": "merge_group"}, [])
        self.assertEqual((0, []), (code, calls))
        self.assert_runs_everything(written)

    def test_failed_fetch_or_verifier_runs_everything(self) -> None:
        for codes in ([1], [0, 2]):
            code, _, written = self.run_main(PR_ENV, codes)
            self.assertEqual(0, code)
            self.assert_runs_everything(written)


class PolicyTests(unittest.TestCase):
    """ci-attestations.yml must describe what the ci-minimal Dylint call runs."""

    def setUp(self) -> None:
        self.policy = yaml.safe_load((ROOT / "ci-attestations.yml").read_text())

    def test_every_job_the_dylint_call_runs_is_mapped(self) -> None:
        for job in ("policy", "dylint", "gate"):
            self.assertIn(f"dylint.yml:{job}", self.policy["jobs"])

    def test_dylint_job_requires_every_target_the_call_lints(self) -> None:
        """ci-minimal passes `ref`, so dylint.yml lints all three targets."""
        gates = self.policy["jobs"]["dylint.yml:dylint"]
        for triple in ("x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc", "x86_64-apple-darwin"):
            self.assertIn(f"rust/{triple}/dylint", gates)
        self.assertEqual(set(gates) | {"general/all/dylint-policy"}, set(self.policy["jobs"]["dylint.yml:gate"]))

    def test_rust_proof_input_keys_cover_every_workspace_member(self) -> None:
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
        for lane in ("lint", "dylint"):
            inputs = self.policy["lanes"][lane]["inputs"]
            for member in workspace["members"]:
                with self.subTest(lane=lane, member=member):
                    self.assertIn(f"{member.split('/', 1)[0]}/**", inputs)

    def test_every_mapped_gate_is_declared(self) -> None:
        for job, gates in self.policy["jobs"].items():
            for gate in gates:
                self.assertIn(gate, self.policy["gates"], f"{job} maps undeclared gate {gate}")


class PushTests(unittest.TestCase):
    """GATE-014/016/017: `./push` delegates to the prooft runner; the policy names the lanes."""

    def setUp(self) -> None:
        self.policy = yaml.safe_load((ROOT / "ci-attestations.yml").read_text())
        self.lanes = self.policy["lanes"]

    def stage(self, lane: str) -> str:
        """prooft's rule: the declared stage, else lint when every gate is a lint check."""
        if "stage" in self.lanes[lane]:
            return self.lanes[lane]["stage"]
        checks = {gate.rsplit("/", 1)[1] for gate, spec in self.policy["gates"].items() if spec["lane"] == lane}
        return "lint" if checks and checks <= LINT_CHECKS else "test"

    def test_push_execs_the_pinned_prooft_runner_with_the_callers_args(self) -> None:
        with patch.object(sys, "argv", ["push", "--test", "--no-pr"]), patch.object(os, "execvp") as execvp:
            load_push()
        argv = prooft_verify.prooft_argv("--push", "--test", "--no-pr")
        execvp.assert_called_once_with(argv[0], argv)
        self.assertEqual(["uvx", "--from", prooft_verify.PROOFT_SOURCE, "prooft"], argv[:4])

    def test_push_never_publishes_attests_or_skips_lint_itself(self) -> None:
        code = "\n".join(
            line for line in (ROOT / "push").read_text().splitlines() if not line.lstrip().startswith("#")
        )
        for banned in ("git push", "gh pr", "subprocess", "local_gate", "--no-lint", '"run"', '"soldr"', '"apply"'):
            self.assertNotIn(banned, code)

    def runnable(self) -> dict[str, list[str]]:
        """Lanes the bare runner executes, in declaration order, with their argv."""
        runs = {lane: spec["run"] for lane, spec in self.lanes.items() if "run" in spec}
        for lane, run in runs.items():
            with self.subTest(lane=lane):
                self.assertTrue(isinstance(run, list) and run and all(isinstance(arg, str) for arg in run))
        return runs

    def test_lint_runs_first_on_the_host_only_through_soldr_lint(self) -> None:
        runs = self.runnable()
        stages = [self.stage(lane) for lane in runs]
        self.assertIn("lint", stages, "GATE-017: at least one runnable lint lane")
        self.assertEqual(sorted(stages, key=lambda stage: stage != "lint"), stages, "lint lanes come first")
        for lane, argv in runs.items():
            if self.stage(lane) == "lint":
                with self.subTest(lane=lane):
                    self.assertEqual(["soldr", "lint"], argv[:2], "GATE-016: lint is host soldr lint, never bosn")
                    self.assertIn("--host-only", argv, "local lint skips cross targets; CI covers them")
                    self.assertNotIn("--cross-dylint", argv)
        self.assertEqual(["soldr", "lint", "rust", "--check", "--host-only"], runs["lint"])

    def test_test_lane_is_the_native_host_replay_and_cross_lanes_stay_remote(self) -> None:
        runs = self.runnable()
        self.assertEqual("test", self.stage("linux-minimal"))
        self.assertEqual(["ci.local_gate", "--lane", "linux-minimal"], runs["linux-minimal"][-3:])
        self.assertIn("glibc-floor", local_gate.UBUNTU_SELECTION.skipped_jobs, "the release cross-link stays remote")
        self.assertNotIn("dylint", runs, "cross-target Dylint is CI's; a host run cannot prove the lane")


if __name__ == "__main__":
    unittest.main()

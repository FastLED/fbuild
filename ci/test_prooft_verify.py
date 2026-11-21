"""Contract tests for ci-minimal's `prooft` job and the `./push` lanes.

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
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

import yaml

from ci import prooft_verify

ROOT = Path(__file__).resolve().parents[1]
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
    @staticmethod
    def git_outputs(branch: str = "feature", status: str = "") -> dict[tuple[str, ...], str]:
        return {
            ("rev-parse", "--abbrev-ref", "HEAD"): branch,
            ("for-each-ref", "--format=%(symref:short)", "refs/remotes/origin/HEAD"): "origin/trunk",
            ("status", "--porcelain", "--untracked-files=no"): status,
            ("ls-files", "--others", "--exclude-standard"): "",
        }

    def test_push_lanes_prove_every_required_gate(self) -> None:
        push = load_push()
        policy = yaml.safe_load((ROOT / "ci-attestations.yml").read_text())
        needed = {gate for gates in policy["jobs"].values() for gate in gates}
        proven = {gate for gate, spec in policy["gates"].items() if spec["lane"] in push.LANES}
        self.assertLessEqual(needed, proven)
        self.assertEqual(set(push.LANES), set(policy["lanes"]))

    def test_refuses_protected_branches_and_dirty_trees(self) -> None:
        push = load_push()
        cases = (("main", ""), ("trunk", ""), ("HEAD", ""), ("feature", " M crates/x.rs"))
        for branch, status in cases:
            outputs = self.git_outputs(branch, status)
            with patch.object(push, "git", lambda *args: outputs[args]), patch.object(push, "attest") as attest:
                self.assertEqual(3, push.main([]))
            attest.assert_not_called()

    def test_no_proof_bypass_is_a_usage_error(self) -> None:
        push = load_push()
        with patch.object(push, "step") as step, self.assertRaises(SystemExit) as raised:
            push.main(["--no-proof"])
        self.assertEqual(2, raised.exception.code)
        step.assert_not_called()

    def test_failed_proof_pushes_nothing(self) -> None:
        push = load_push()
        outputs = self.git_outputs()
        with (
            patch.object(push, "git", lambda *args: outputs[args]),
            patch.object(push, "attest", return_value=False),
            patch.object(push, "step") as step,
        ):
            self.assertEqual(1, push.main([]))
        step.assert_not_called()

    def test_lanes_verify_before_push_and_pr(self) -> None:
        push = load_push()
        calls = []
        outputs = self.git_outputs()

        def git(*args, **_kwargs):
            return "" if args[0] == "update-ref" else outputs[args]

        def run(argv):
            calls.append(argv)
            return 0

        gates = {lane: {lane} for lane in push.LANES}
        with (
            patch.object(push, "git", git),
            patch.object(push, "lane_gates", return_value=gates),
            patch.object(push, "reuse_proof"),
            patch.object(push, "proof_status", side_effect=[set(), {"lint"}, set(gates)]),
            patch.object(push, "step", run),
            patch.object(push, "has_open_pr", return_value=False),
        ):
            self.assertEqual(0, push.main([]))
        self.assertEqual([
            ["run", "linux-minimal"], ["run", "dylint"], ["verify", "HEAD"],
        ], [argv[6:8] for argv in calls[:3]])
        self.assertEqual(["git", "push"], calls[3][:2])
        self.assertEqual(["gh", "pr", "create"], calls[4][:3])

    def test_missing_gate_after_verify_refuses_push(self) -> None:
        push = load_push()
        gates = {lane: {lane} for lane in push.LANES}
        with (
            patch.object(push, "git"),
            patch.object(push, "lane_gates", return_value=gates),
            patch.object(push, "reuse_proof"),
            patch.object(push, "proof_status", return_value={"lint"}),
            patch.object(push, "step", return_value=0),
        ):
            self.assertFalse(push.attest("feature"))


if __name__ == "__main__":
    unittest.main()

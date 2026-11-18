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
import unittest
from pathlib import Path
from unittest.mock import patch

import yaml

from ci import prooft_verify

ROOT = Path(__file__).resolve().parents[1]
PR_ENV = {"EVENT_NAME": "pull_request", "BASE_SHA": "b" * 40, "HEAD_SHA": "h" * 40, "PR_TITLE": "t", "FORK": "false"}


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
        for event in ("merge_group", "push", "workflow_dispatch"):
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
        self.assertEqual(["git", "fetch", "-q", "origin", "h" * 40], calls[0])
        plan = calls[1]
        self.assertEqual(["uvx", "--from", prooft_verify.PROOFT_SOURCE, "prooft"], plan[:4])
        self.assertRegex(prooft_verify.PROOFT_SOURCE, r"@[0-9a-f]{40}$")
        for flag, value in (("--base", "b" * 40), ("--head", "h" * 40), ("--title", "t")):
            self.assertEqual(value, plan[plan.index(flag) + 1])
        self.assertIn("--github-output", plan)

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
        for job in prooft_verify.JOBS:
            self.assertIn(f"dylint.yml:{job}", self.policy["jobs"])

    def test_dylint_job_requires_every_target_the_call_lints(self) -> None:
        """ci-minimal passes `ref`, so dylint.yml lints all three targets."""
        gates = self.policy["jobs"]["dylint.yml:dylint"]
        for triple in ("x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc", "x86_64-apple-darwin"):
            self.assertIn(f"rust/{triple}/dylint", gates)
        self.assertEqual(set(gates) | {"general/all/dylint-policy"}, set(self.policy["jobs"]["dylint.yml:gate"]))

    def test_every_mapped_gate_is_declared(self) -> None:
        for job, gates in self.policy["jobs"].items():
            for gate in gates:
                self.assertIn(gate, self.policy["gates"], f"{job} maps undeclared gate {gate}")


class PushTests(unittest.TestCase):
    def test_push_lanes_prove_every_gate_the_dylint_call_needs(self) -> None:
        push = load_push()
        policy = yaml.safe_load((ROOT / "ci-attestations.yml").read_text())
        needed = {g for job in prooft_verify.JOBS for g in policy["jobs"][f"dylint.yml:{job}"]}
        proven = {g for g, spec in policy["gates"].items() if spec["lane"] in push.LANES}
        self.assertLessEqual(needed, proven)

    def test_lint_lane_is_the_cross_target_soldr_lint(self) -> None:
        push = load_push()
        self.assertEqual(["soldr", "--", "soldr", "lint", "rust", "--check", "--cross-dylint"], push.LANES["lint"])

    def test_refuses_protected_branches_and_dirty_trees(self) -> None:
        push = load_push()
        for branch, dirty in (("main", ""), ("feature", " M crates/x.rs")):
            outputs = {("rev-parse", "--abbrev-ref", "HEAD"): branch, ("status", "--porcelain", "--untracked-files=normal"): dirty}
            with patch.object(push, "git", lambda *a, o=outputs: o[a]), patch.object(push, "step") as step:
                self.assertEqual(3, push.main([]))
            step.assert_not_called()

    def test_untracked_files_refuse_a_proof_but_not_a_plain_push(self) -> None:
        # The dylint lane's local gate refuses any untracked file, so proving
        # with one present burns the whole lint lane first and then fails.
        push = load_push()
        outputs = {("rev-parse", "--abbrev-ref", "HEAD"): "feature", ("status", "--porcelain", "--untracked-files=normal"): "?? notes.txt"}
        with patch.object(push, "git", lambda *a: outputs[a]), patch.object(push, "step") as step:
            self.assertEqual(3, push.main([]))
        step.assert_not_called()
        with patch.object(push, "git", lambda *a: outputs[a]), patch.object(push, "step", return_value=0) as step, patch.object(push, "has_open_pr", return_value=True):
            self.assertEqual(0, push.main(["--no-proof"]))
        self.assertIn("push", step.call_args_list[0][0][0])

    def test_failed_proof_pushes_nothing(self) -> None:
        push = load_push()
        outputs = {("rev-parse", "--abbrev-ref", "HEAD"): "feature", ("status", "--porcelain", "--untracked-files=normal"): ""}
        with patch.object(push, "git", lambda *a: outputs[a]), patch.object(push, "step", return_value=1) as step:
            self.assertEqual(1, push.main([]))
        self.assertEqual(1, step.call_count)
        self.assertNotIn("push", step.call_args[0][0])


if __name__ == "__main__":
    unittest.main()

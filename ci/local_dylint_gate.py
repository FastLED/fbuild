"""Replay every job of the Dylint workflow the PR path actually runs.

`dylint.yml` lost its own `pull_request`/`merge_group` triggers in #79a5200a:
PRs, the merge queue and main pushes all reach it through a caller's
`workflow_call` (ci-minimal, ci-full). Replaying the file under
`pull_request` therefore selected no stages at all ("act: Error: Could not
find any stages to run"), which is FastLED/fbuild#2086.

The caller call site cannot be replayed here: selecting `ci-minimal.yml:dylint`
drags in its `prooft` dependency, whose checkout/setup steps have no literal
names, so no declaration form can attest it (and its output-driven `if:`
needs the deferred-output producer). `dylint.yml` keeps its
`workflow_dispatch` trigger for exactly this purpose, and the local
`linux-minimal` lane already replays `ci-minimal.yml` the same way. Under
dispatch the file's own `FULL_TARGETS` expression makes "Run dylint over
workspace" cover all three OS targets, which is what the PR path runs through
ci-minimal's call -- the old pull_request selection was Linux-only, so the
replay is now a superset of the proof it used to claim: same jobs, same steps.
"""

from __future__ import annotations

import json
import os
import subprocess
from dataclasses import dataclass
from pathlib import Path

from ci.local_gate import (
    JsonValue,
    RunProof,
    RunSelection,
    bosn_command,
    document,
    output,
    submitted_run_id,
    verify_run,
    wire_string,
)

SELECTION = RunSelection(
    ".github/workflows/dylint.yml",
    None,
    "workflow_dispatch",
    frozenset({"policy", "dylint", "gate"}),
    # The end-of-queue cache janitor is confined to `merge_group` by its own
    # guard, so a dispatch replay reports it explicitly skipped.
    frozenset({"queue-cache-janitor"}),
)


@dataclass(frozen=True)
class RequiredSteps:
    job: str
    names: tuple[str, ...]


REQUIRED_STEPS = (
    RequiredSteps("policy", ("Validate Dylint policy files",)),
    RequiredSteps(
        "dylint",
        (
            "Validate Dylint allowlist paths",
            "Enforce shrink-only .fbuild allowlist",
            "Validate platform-boundary ledgers",
            "Prepare prebuilt Dylint tools and driver",
            "Install rustfmt for the Dylint toolchain",
            "Check Dylint library formatting",
            "Test Dylint libraries",
            "Run dylint over workspace",
        ),
    ),
)


@dataclass(frozen=True)
class StepProof:
    job: str
    name: str
    stage: str
    status: str
    conclusion: str | None


def read_steps(raw: dict[str, JsonValue]) -> tuple[StepProof, ...]:
    tree = raw.get("tree")
    if not isinstance(tree, dict) or not isinstance(tree.get("groups"), list):
        raise ValueError("Dylint proof lacks job groups")
    steps: list[StepProof] = []
    for group in tree["groups"]:
        if not isinstance(group, dict) or not isinstance(group.get("jobs"), list):
            raise ValueError("invalid Dylint job group")
        for job in group["jobs"]:
            if not isinstance(job, dict) or not isinstance(job.get("sections"), list):
                raise ValueError("Dylint proof lacks step evidence")
            job_id = wire_string(job, "job_id")
            for section in job["sections"]:
                if not isinstance(section, dict):
                    raise ValueError("invalid Dylint step evidence")
                conclusion = section.get("conclusion")
                if conclusion is not None and not isinstance(conclusion, str):
                    raise ValueError("invalid Dylint step conclusion")
                steps.append(
                    StepProof(
                        job_id,
                        *(
                            wire_string(section, key)
                            for key in ("name", "stage", "status")
                        ),
                        conclusion,
                    )
                )
    return tuple(steps)


def verify_dylint(raw: dict[str, JsonValue], workspace: Path, sha: str) -> None:
    verify_run(RunProof.from_json(raw), workspace, sha, SELECTION)
    steps = read_steps(raw)
    for required in REQUIRED_STEPS:
        for name in required.names:
            matching = [
                step
                for step in steps
                if step.job == required.job
                and step.name == name
                and step.stage == "Main"
            ]
            if (
                len(matching) != 1
                or matching[0].status != "completed"
                or matching[0].conclusion != "success"
            ):
                raise ValueError(
                    f"required Dylint step did not pass: {required.job}/{name}"
                )


def run_dylint(workspace: Path, sha: str) -> None:
    submitted = document(
        bosn_command(
            "ci",
            "run",
            "--workspace",
            str(workspace),
            "--workflow",
            SELECTION.workflow,
            "--event",
            "workflow_dispatch",
            "--mode",
            "minimal",
            "--sha",
            sha,
            "--timeout-secs",
            "7200",
            "--json",
        )
    )
    run_id = submitted_run_id(submitted)
    print(f"bosn dispatch Dylint run: {run_id}", flush=True)
    subprocess.run(bosn_command("ci", "wait", run_id), cwd=workspace, check=True)
    report = output(bosn_command("ci", "show", run_id, "--json"))
    raw: JsonValue = json.loads(report)
    if not isinstance(raw, dict):
        raise ValueError("bosn returned a non-object Dylint report")
    verify_dylint(raw, workspace, sha)
    report_path = os.environ.get("CI_LINT_GATE_REPLAY_REPORT")
    if report_path:
        Path(report_path).write_text(report, encoding="utf-8")

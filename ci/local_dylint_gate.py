"""Replay the existing ordinary-PR Dylint workflow without selecting a job."""

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
    verify_run,
    wire_string,
)

SELECTION = RunSelection(
    ".github/workflows/dylint.yml",
    None,
    "pull_request",
    frozenset({"policy", "dylint", "gate"}),
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
            "--trigger",
            "pr",
            "--mode",
            "minimal",
            "--sha",
            sha,
            "--timeout-secs",
            "7200",
            "--json",
        )
    )
    run_id = wire_string(submitted, "run")
    print(f"bosn ordinary-PR Dylint run: {run_id}", flush=True)
    subprocess.run(bosn_command("ci", "wait", run_id), cwd=workspace, check=True)
    report = output(bosn_command("ci", "show", run_id, "--json"))
    raw: JsonValue = json.loads(report)
    if not isinstance(raw, dict):
        raise ValueError("bosn returned a non-object Dylint report")
    verify_dylint(raw, workspace, sha)
    report_path = os.environ.get("CI_LINT_GATE_REPLAY_REPORT")
    if report_path:
        Path(report_path).write_text(report, encoding="utf-8")

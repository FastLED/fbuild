"""Run both required Ubuntu jobs through bosn's pinned act2 engine.

This proof covers native Linux x64 only. Board builds, extended/full mode and
the other native hosts retain their remote validation.
"""

from __future__ import annotations

import json
import platform
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path

JsonValue = str | int | float | bool | None | list["JsonValue"] | dict[str, "JsonValue"]
ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ".github/workflows/ci-minimal.yml"
REQUIRED_JOBS = frozenset({"check", "python-facade-tests"})


def wire_string(raw: dict[str, JsonValue], key: str) -> str:
    value = raw.get(key)
    if not isinstance(value, str):
        raise ValueError(f"bosn record lacks string field {key}")
    return value


def wire_count(raw: dict[str, JsonValue], key: str) -> int:
    value = raw.get(key)
    if type(value) is not int or value < 0:
        raise ValueError(f"bosn record lacks nonnegative integer field {key}")
    return value


@dataclass(frozen=True)
class JobProof:
    job_id: str
    status: str
    conclusion: str


@dataclass(frozen=True)
class RunProof:
    workspace: Path
    sha: str
    dirty: JsonValue
    engine: str
    act_version: str
    workflow: str
    job: str
    mode: str
    event: str
    state: str
    conclusion: str
    exit_code: int
    total: int
    completed: int
    failed: int
    jobs: tuple[JobProof, ...]

    @classmethod
    def from_json(cls, raw: dict[str, JsonValue]) -> RunProof:
        counts = raw.get("jobs")
        tree = raw.get("tree")
        if (
            not isinstance(counts, dict)
            or not isinstance(tree, dict)
            or "dirty" not in raw
        ):
            raise ValueError("bosn record lacks snapshot or job evidence")
        groups = tree.get("groups")
        if not isinstance(groups, list):
            raise ValueError("bosn record lacks job groups")
        if wire_count(tree, "malformed_lines") != 0:
            raise ValueError("bosn job evidence contains malformed log records")
        jobs: list[JobProof] = []
        for group in groups:
            if not isinstance(group, dict) or not isinstance(group.get("jobs"), list):
                raise ValueError("invalid bosn job group")
            for job in group["jobs"]:
                if not isinstance(job, dict):
                    raise ValueError("invalid bosn job record")
                jobs.append(
                    JobProof(
                        *(
                            wire_string(job, key)
                            for key in ("job_id", "status", "conclusion")
                        )
                    )
                )
        return cls(
            Path(wire_string(raw, "workspace")).resolve(),
            wire_string(raw, "sha"),
            raw["dirty"],
            *(
                wire_string(raw, key)
                for key in (
                    "engine",
                    "act_version",
                    "workflow",
                    "job",
                    "mode",
                    "event",
                    "state",
                    "conclusion",
                )
            ),
            wire_count(raw, "exit_code"),
            *(wire_count(counts, key) for key in ("total", "completed", "failed")),
            tuple(jobs),
        )


def verify_run(proof: RunProof, workspace: Path, sha: str) -> None:
    if (
        proof.workspace != workspace.resolve()
        or proof.sha != sha
        or proof.dirty is not None
        or proof.engine != "act"
        or "-act2." not in proof.act_version
        or proof.workflow != WORKFLOW
        or proof.job != "linux"
        or proof.mode != "minimal"
        or proof.event != "workflow_dispatch"
    ):
        raise ValueError("run does not prove this clean minimal Linux source")
    if (
        proof.state != "done"
        or proof.conclusion != "success"
        or proof.exit_code != 0
        or proof.total < 2
        or proof.completed != proof.total
        or proof.failed != 0
        or len(proof.jobs) != proof.total
    ):
        raise ValueError("selected jobs did not all complete successfully")
    for job_id in REQUIRED_JOBS:
        matching = [job for job in proof.jobs if job.job_id == job_id]
        if (
            len(matching) != 1
            or matching[0].status != "completed"
            or matching[0].conclusion != "success"
        ):
            raise ValueError(f"required Ubuntu job {job_id} did not pass")
    if any(
        job.status != "completed" or job.conclusion != "success" for job in proof.jobs
    ):
        raise ValueError("selected job evidence includes a non-successful job")


def output(argv: list[str]) -> str:
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as stream:
        subprocess.run(argv, cwd=ROOT, stdout=stream, check=True)
        stream.seek(0)
        return stream.read()


def document(argv: list[str]) -> dict[str, JsonValue]:
    raw: JsonValue = json.loads(output(argv))
    if not isinstance(raw, dict):
        raise ValueError("bosn returned a non-object document")
    return raw


def main() -> None:
    if (
        output(["git", "symbolic-ref", "refs/remotes/origin/HEAD"]).strip()
        != "refs/remotes/origin/main"
    ):
        raise ValueError("origin/HEAD must name main, not this feature branch")
    if output(["git", "status", "--porcelain", "--untracked-files=normal"]).strip():
        raise ValueError("commit the worktree before running the local gate")
    subprocess.run(
        [
            "uv",
            "run",
            "--no-project",
            "--with",
            "pyyaml==6.0.2",
            "python",
            "-m",
            "unittest",
            "ci.test_local_gate",
            "ci.test_fractional_workflows",
            "ci.test_test_cache_status",
        ],
        cwd=ROOT,
        check=True,
    )
    subprocess.run(
        [
            "uv",
            "run",
            "--no-project",
            "--with",
            "pyyaml==6.0.2",
            "python",
            "-c",
            "import pathlib,yaml; [yaml.safe_load(pathlib.Path(p).read_text()) for p in ('.github/workflows/ci-minimal.yml','.github/workflows/check-ubuntu.yml')]",
        ],
        cwd=ROOT,
        check=True,
    )
    daemon = output(["docker", "info", "--format", "{{.OSType}}/{{.Architecture}}"])
    if (
        platform.system() != "Linux"
        or platform.machine().lower() not in {"x86_64", "amd64"}
        or daemon.strip().lower() not in {"linux/x86_64", "linux/amd64"}
    ):
        raise ValueError("Linux x64 tests require a native Linux x64 host and daemon")
    sha = output(["git", "rev-parse", "HEAD"]).strip()
    submitted = document(
        [
            "bosn",
            "ci",
            "run",
            "--workspace",
            str(ROOT),
            "--workflow",
            WORKFLOW,
            "--job",
            "linux",
            "--event",
            "workflow_dispatch",
            "--mode",
            "minimal",
            "--sha",
            sha,
            "--timeout-secs",
            "7200",
            "--json",
        ]
    )
    run_id = wire_string(submitted, "run")
    print(f"bosn local gate run: {run_id}", flush=True)
    subprocess.run(["bosn", "ci", "wait", run_id], cwd=ROOT, check=True)
    verify_run(
        RunProof.from_json(document(["bosn", "ci", "show", run_id, "--json"])),
        ROOT,
        sha,
    )
    if (
        output(["git", "rev-parse", "HEAD"]).strip() != sha
        or output(["git", "status", "--porcelain", "--untracked-files=normal"]).strip()
    ):
        raise ValueError("worktree changed while the local gate ran")
    print(f"Passed both required Ubuntu jobs: {sha}", flush=True)


if __name__ == "__main__":
    main()

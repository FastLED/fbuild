"""Run the local gate: the Ubuntu workflow replay plus host lint lanes.

The `linux-minimal` lane replays `check-ubuntu.yml` through act2. Lint never
runs under bosn: the `dylint` and `lint` lanes run `soldr lint` on the host.
This proof covers native Linux x64 only. Board builds, extended/full mode and
the other native hosts retain their remote validation.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path

from ci.local_suites import LOCAL_UNITTEST_MODULES

JsonValue = str | int | float | bool | None | list["JsonValue"] | dict[str, "JsonValue"]
ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ".github/workflows/check-ubuntu.yml"
REQUIRED_JOBS = frozenset({"check", "python-facade-tests"})


@dataclass(frozen=True)
class RunSelection:
    """One replay invocation and the verdicts it must produce.

    `skipped_jobs` names jobs the workflow itself confines to another event
    (dylint.yml's `merge_group`-only cache janitor). act2 still reports them,
    explicitly skipped, so a proof that demanded every reported job succeed
    could never accept the run.
    """

    workflow: str
    job: str | None
    event: str
    required_jobs: frozenset[str]
    skipped_jobs: frozenset[str] = frozenset()


UBUNTU_SELECTION = RunSelection(WORKFLOW, None, "workflow_dispatch", REQUIRED_JOBS, frozenset({"glibc-floor"}))


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
    job: str | None
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
            if not isinstance(group, dict) or not isinstance(
                group_jobs := group.get("jobs"), list
            ):
                raise ValueError("invalid bosn job group")
            for job in group_jobs:
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
        selected_job = raw.get("job")
        if "job" not in raw or (
            selected_job is not None and not isinstance(selected_job, str)
        ):
            raise ValueError("bosn record lacks a valid job selection")
        return cls(
            Path(wire_string(raw, "workspace")).resolve(),
            wire_string(raw, "sha"),
            raw["dirty"],
            wire_string(raw, "engine"),
            wire_string(raw, "act_version"),
            wire_string(raw, "workflow"),
            selected_job,
            wire_string(raw, "mode"),
            wire_string(raw, "event"),
            wire_string(raw, "state"),
            wire_string(raw, "conclusion"),
            wire_count(raw, "exit_code"),
            wire_count(counts, "total"),
            wire_count(counts, "completed"),
            wire_count(counts, "failed"),
            tuple(jobs),
        )


def expected_job_count(selection: RunSelection) -> int:
    return len(selection.required_jobs) + len(selection.skipped_jobs)


def verify_run(
    proof: RunProof,
    workspace: Path,
    sha: str,
    selection: RunSelection = UBUNTU_SELECTION,
) -> None:
    if (
        proof.workspace != workspace.resolve()
        or proof.sha != sha
        or proof.dirty is not None
        or proof.engine != "act"
        or "-act2." not in proof.act_version
        or proof.workflow != selection.workflow
        or proof.job != selection.job
        or proof.mode != "minimal"
        or proof.event != selection.event
    ):
        raise ValueError("run does not prove this clean minimal Linux source")
    if (
        proof.state != "done"
        or proof.conclusion != "success"
        or proof.exit_code != 0
        or proof.total != expected_job_count(selection)
        or proof.completed != proof.total
        or proof.failed != 0
        or len(proof.jobs) != proof.total
    ):
        raise ValueError("selected jobs did not all complete successfully")
    for job_id in selection.required_jobs:
        matching = [job for job in proof.jobs if job.job_id == job_id]
        if (
            len(matching) != 1
            or matching[0].status != "completed"
            or matching[0].conclusion != "success"
        ):
            raise ValueError(f"required Ubuntu job {job_id} did not pass")
    for job_id in selection.skipped_jobs:
        matching = [job for job in proof.jobs if job.job_id == job_id]
        if (
            len(matching) != 1
            or matching[0].status != "completed"
            or matching[0].conclusion != "skipped"
        ):
            raise ValueError(f"event-excluded job {job_id} did not skip")
    if any(
        job.status != "completed"
        or (
            job.conclusion != "success"
            and job.job_id not in selection.skipped_jobs
        )
        for job in proof.jobs
    ):
        raise ValueError("selected job evidence includes a non-successful job")


def submitted_run_id(reply: dict[str, JsonValue]) -> str:
    """The run id from `bosn ci run --json`.

    bosn 0.1.22 answers with `{"run": "<id>"}`; 0.1.32 waits for the run and
    answers with its terminal report, which carries the same id as `"id"`.
    """
    for key in ("run", "id"):
        value = reply.get(key)
        if isinstance(value, str) and value:
            return value
    raise ValueError("bosn submit reply lacks a run id")


def bosn_command(*arguments: str) -> list[str]:
    """Use the latest released runner and an optional session-owned daemon root.

    A floor, not an exact pin: the shared bosn daemon refuses clients from a
    different release, so an exact pin breaks the gate as soon as any other
    session starts a newer daemon. `--refresh-package` keeps uvx from reusing
    a stale cached resolution.
    """
    argv = [
        "uvx",
        "--refresh-package",
        "bosn",
        "--from",
        "bosn>=0.1.32",
        "bosn",
        *arguments,
    ]
    state = os.environ.get("BOSN_GATE_STATE_DIR")
    if state:
        argv.extend(["--state-dir", str(Path(state).resolve())])
    return argv


def output(argv: list[str]) -> str:
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as stream:
        result = subprocess.run(argv, cwd=ROOT, stdout=stream, check=False)
        stream.seek(0)
        text = stream.read()
        if result.returncode:
            print(text, file=sys.stderr, end="")
            result.check_returncode()
        return text


def document(argv: list[str]) -> dict[str, JsonValue]:
    raw: JsonValue = json.loads(output(argv))
    if not isinstance(raw, dict):
        raise ValueError("bosn returned a non-object document")
    return raw


def load_aware_test_threads(cpus: int, load: float) -> int:
    cpus = max(1, cpus)
    return max(1, int(cpus * cpus / max(cpus, load)))


def replay_concurrency() -> list[str]:
    threads = load_aware_test_threads(len(os.sched_getaffinity(0)), os.getloadavg()[0])
    print(f"Local replay concurrency: {threads} jobs and test threads", flush=True)
    return ["--env", f"CARGO_BUILD_JOBS={threads}", "--env", f"RUST_TEST_THREADS={threads}"]


def run_ubuntu(sha: str) -> None:
    submitted = document(
        bosn_command(
            "ci",
            "run",
            "--workspace",
            str(ROOT),
            "--workflow",
            WORKFLOW,
            "--event",
            UBUNTU_SELECTION.event,
            "--input",
            "glibc_floor=false",
            "--input",
            f"ref={sha}",
            *replay_concurrency(),
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
    print(f"bosn local gate run: {run_id}", flush=True)
    subprocess.run(bosn_command("ci", "wait", run_id), cwd=ROOT, check=True)
    report = output(bosn_command("ci", "show", run_id, "--json"))
    raw: JsonValue = json.loads(report)
    if not isinstance(raw, dict):
        raise ValueError("bosn returned a non-object terminal report")
    verify_run(RunProof.from_json(raw), ROOT, sha)
    report_path = os.environ.get("CI_LINT_GATE_REPLAY_REPORT")
    if report_path:
        Path(report_path).write_text(report, encoding="utf-8")


def validate_ci_scripts() -> None:
    source = tomllib.loads((ROOT / "ci.toml").read_text(encoding="utf-8"))["linter"]
    subprocess.run(
        [
            "uv",
            "run",
            "--no-project",
            "--with",
            "pyyaml==6.0.2",
            "--with",
            "zstandard>=0.23",
            "--with",
            f"ci-lint @ git+https://github.com/{source}",
            "python",
            "-m",
            "unittest",
            *LOCAL_UNITTEST_MODULES,
        ],
        cwd=ROOT,
        check=True,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lane", choices=("linux-minimal", "dylint", "lint"))
    lane = parser.parse_args().lane
    if (
        output(["git", "symbolic-ref", "refs/remotes/origin/HEAD"]).strip()
        != "refs/remotes/origin/main"
    ):
        raise ValueError("origin/HEAD must name main, not this feature branch")
    if output(["git", "status", "--porcelain", "--untracked-files=normal"]).strip():
        raise ValueError("commit the worktree before running the local gate")
    if lane is None:
        validate_ci_scripts()
    if platform.system() != "Linux" or platform.machine().lower() not in {"x86_64", "amd64"}:
        raise ValueError("the local gate requires a native Linux x64 host")
    sha = output(["git", "rev-parse", "HEAD"]).strip()
    if lane in (None, "linux-minimal"):
        daemon = output(["docker", "info", "--format", "{{.OSType}}/{{.Architecture}}"])
        if daemon.strip().lower() not in {"linux/x86_64", "linux/amd64"}:
            raise ValueError("the Ubuntu replay requires a native Linux x64 daemon")
        run_ubuntu(sha)
    from ci.local_dylint_gate import run_dylint

    # Host lane: lint never runs under bosn (soldr lint only).
    if lane in (None, "dylint"):
        run_dylint(ROOT)
    from ci.local_lint_gate import run_lint

    # Opt-in for now: the cold run is expensive, and GATE-007's cache key only
    # skips a byte-identical tree. Promote to `lane in (None, "lint")` once a
    # warm run is measured at <= max(30 s, 10% of cold) per fleet policy RUST-033.
    if lane == "lint":
        run_lint()
    if (
        output(["git", "rev-parse", "HEAD"]).strip() != sha
        or output(["git", "status", "--porcelain", "--untracked-files=normal"]).strip()
    ):
        raise ValueError("worktree changed while the local gate ran")
    print(
        f"Passed the selected native Linux workflow coverage: {sha}",
        flush=True,
    )


if __name__ == "__main__":
    main()

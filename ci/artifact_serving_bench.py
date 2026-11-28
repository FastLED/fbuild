#!/usr/bin/env python3
"""The artifact-serving cold-build benchmark: two arms, one report, one gate.

Decision 6 of `docs/design/artifact-serving-decisions.md` is the last cell of
FastLED/fbuild#1882, and FastLED/fbuild#2196 is the consumer it was waiting for.
It asks for one headline number *with its control arm*:

    cold_first_compile_wall_s          (framework core objects served)
    cold_first_compile_wall_s_source   (``--no-prebuilt-libs`` control, same job)
    speedup_ratio                      source / served

and for a gate: a reference-cell regression above **10 %** fails the check. Raw
JSON is the artifact of record; the page is generated from it later.

Why the control arm is not optional: the two arms are only tellable apart by
what the build itself says it did, and that is now recorded -- cell 4 of #2196
writes `prebuilt_libs: {used, source, key, sha256, fingerprint, reason}` into
`build_info_<env>.json` on every ESP32 build (compare
`crates/fbuild-build-engine/src/prebuilt_record.rs`). A served arm whose record
says `used: false` fell back to source, and a ratio computed from it would read
as a 1.00x "speedup" while measuring nothing. Decision 6 calls that a hard
failure, so this module refuses to produce a report at all: a fallback is not a
measurement.

Cold means cold: every trial runs with its own empty `FBUILD_CACHE_DIR`, so a
trial cannot inherit the previous trial's extracted framework. The caller passes
the project directory; this module never creates a sketch, because the reference
cell must be the same project the site publishes.

Usage (from the repository root, so ``ci`` is importable as a package):

    python3 -m ci.artifact_serving_bench --measure \\
        --project /path/to/reference-cell --env esp32s3 \\
        --trials 3 --out benchmark-measurements/artifact-serving --sha <sha>

    python3 -m ci.artifact_serving_bench --check \\
        --measurements benchmark-measurements/artifact-serving --fail

Remaining for #1882 cell 6 (deliberately out of this module's scope): the
nightly workflow step that invokes `--measure`, and the two metrics that need
harness-level instrumentation -- `bytes_downloaded` per arm (a network counter)
and `fallback_rung_histogram` (the ladder does not expose rung numbers yet).
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

SCHEMA = 1
"""Schema of the measurement document; a reader refuses anything else."""

REFERENCE_CELL_CHIP = "esp32s3"
REFERENCE_PLATFORM_VERSION = "framework-arduinoespressif32-libs/core-3.3.12"
REFERENCE_OS = "ubuntu-22.04"

REGRESSION_BUDGET = 0.10
"""Decision 6: a reference-cell regression **above** 10 % fails the check."""

DATE_FORMAT_LENGTH = len("2026-11-28")
SERVED_FLAG = "--prebuilt-core"
SOURCE_FLAG = "--no-prebuilt-libs"
ARMS = ("served", "source")


class ReportError(RuntimeError):
    """A measurement document is not one this module can read."""


class ArmError(RuntimeError):
    """One arm could not be measured, so there is no ratio to report."""


@dataclass(frozen=True)
class Cell:
    """One `(platform-version, chip, OS)` cell of the benchmark."""

    chip: str
    platform_version: str
    os: str

    def as_json(self) -> dict[str, str]:
        return {
            "chip": self.chip,
            "platform_version": self.platform_version,
            "os": self.os,
        }

    @classmethod
    def from_json(cls, value: object) -> Cell:
        document = parse_object(value, "cell")
        return cls(
            chip=parse_text(document.get("chip"), "cell.chip"),
            platform_version=parse_text(
                document.get("platform_version"), "cell.platform_version"
            ),
            os=parse_text(document.get("os"), "cell.os"),
        )


REFERENCE_CELL = Cell(
    chip=REFERENCE_CELL_CHIP,
    platform_version=REFERENCE_PLATFORM_VERSION,
    os=REFERENCE_OS,
)


@dataclass(frozen=True)
class Arm:
    """Every trial of one arm, plus what the builds themselves recorded."""

    wall_s: tuple[float, ...]
    bytes_downloaded: int | None
    prebuilt: dict[str, object] | None

    @property
    def p50_s(self) -> float:
        if not self.wall_s:
            raise ReportError("an arm with no trials has no p50")
        return statistics.median(self.wall_s)

    def as_json(self) -> dict[str, object]:
        return {
            "wall_s": list(self.wall_s),
            "p50_s": round(self.p50_s, 3),
            "bytes_downloaded": self.bytes_downloaded,
            "prebuilt": self.prebuilt,
        }

    @classmethod
    def from_json(cls, value: object, name: str) -> Arm:
        document = parse_object(value, f"arms.{name}")
        trials = document.get("wall_s")
        if not isinstance(trials, list) or not trials:
            raise ReportError(f"arms.{name}.wall_s must be a non-empty list of seconds")
        wall = tuple(seconds(entry, f"arms.{name}.wall_s[]") for entry in trials)
        downloaded = document.get("bytes_downloaded")
        if downloaded is not None:
            if isinstance(downloaded, bool) or not isinstance(downloaded, int):
                raise ReportError(
                    f"arms.{name}.bytes_downloaded must be an integer or null"
                )
        prebuilt = document.get("prebuilt")
        if prebuilt is not None:
            prebuilt = parse_object(prebuilt, f"arms.{name}.prebuilt")
        return cls(wall_s=wall, bytes_downloaded=downloaded, prebuilt=prebuilt)


@dataclass(frozen=True)
class Report:
    """One benchmark run: both arms of one cell, at one point in time."""

    generated_at: str
    sha: str | None
    cell: Cell
    served: Arm
    source: Arm

    def metrics(self) -> dict[str, float]:
        served = self.served.p50_s
        source = self.source.p50_s
        return {
            "cold_first_compile_wall_s": round(served, 3),
            "cold_first_compile_wall_s_source": round(source, 3),
            "speedup_ratio": round(source / served, 3),
        }

    def as_json(self) -> dict[str, object]:
        return {
            "schema": SCHEMA,
            "generated_at": self.generated_at,
            "sha": self.sha,
            "cell": self.cell.as_json(),
            "trials": max(len(self.served.wall_s), len(self.source.wall_s)),
            "arms": {"served": self.served.as_json(), "source": self.source.as_json()},
            "metrics": self.metrics(),
        }

    @classmethod
    def from_json(cls, value: object) -> Report:
        document = parse_object(value, "measurement document")
        schema = document.get("schema")
        if schema != SCHEMA:
            raise ReportError(
                f"measurement document declares schema {schema!r}, not {SCHEMA}"
            )
        arms = parse_object(document.get("arms"), "arms")
        missing = [name for name in ARMS if name not in arms]
        if missing:
            raise ReportError(f"measurement document has no {', '.join(missing)} arm")
        sha = document.get("sha")
        if sha is not None and not isinstance(sha, str):
            raise ReportError("sha must be a string or null")
        return cls(
            generated_at=parse_text(document.get("generated_at"), "generated_at"),
            sha=sha,
            cell=Cell.from_json(document.get("cell")),
            served=Arm.from_json(arms["served"], "served"),
            source=Arm.from_json(arms["source"], "source"),
        )


def parse_object(value: object, what: str) -> dict[str, object]:
    """`value` as a string-keyed object, or a [`ReportError`] naming it."""
    if not isinstance(value, dict):
        raise ReportError(f"{what} is not a JSON object")
    return {str(key): entry for key, entry in value.items()}


def parse_text(value: object, what: str) -> str:
    if not isinstance(value, str) or not value:
        raise ReportError(f"{what} must be a non-empty string")
    return value


def seconds(value: object, what: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ReportError(f"{what} is not a number")
    measured = float(value)
    if measured <= 0.0:
        raise ReportError(f"{what} must be positive")
    return measured


def load_report(path: Path) -> Report:
    """Read one measurement document; a foreign or truncated one is refused."""
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ReportError(f"{path}: {error}") from error
    return Report.from_json(document)


def measure(
    *,
    fbuild: Path,
    project: Path,
    env_name: str,
    cell: Cell,
    trials: int,
    out_dir: Path,
    environment: dict[str, str] | None = None,
    sleep_between_trials_s: float = 2.0,
    sha: str | None = None,
) -> Report:
    """Run both arms of `cell` and return an unwritten [`Report`].

    Every trial gets its own empty cache root, so `cold_first_compile_wall_s`
    measures a first compile and not a framework reuse. The served arm must
    report `prebuilt_libs.used: true`; a fallback raises [`ArmError`] rather
    than quietly producing a 1.00x row (decision 6).
    """
    if trials < 1:
        raise ArmError("a measurement needs at least one trial")
    measured: dict[str, Arm] = {}
    for name, flag in (("served", SERVED_FLAG), ("source", SOURCE_FLAG)):
        wall: list[float] = []
        record: dict[str, object] | None = None
        for index in range(trials):
            if index and sleep_between_trials_s > 0.0:
                time.sleep(sleep_between_trials_s)
            with tempfile.TemporaryDirectory(prefix=f"fbuild-serving-{name}-") as cache:
                wall.append(
                    run_trial(fbuild, project, env_name, name, flag, cache, environment)
                )
            record = read_prebuilt_record(project, env_name, name)
            enforce_arm(name, record)
        measured[name] = Arm(wall_s=tuple(wall), bytes_downloaded=None, prebuilt=record)
    return Report(
        generated_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        sha=sha,
        cell=cell,
        served=measured["served"],
        source=measured["source"],
    )


def run_trial(
    fbuild: Path,
    project: Path,
    env_name: str,
    arm: str,
    flag: str,
    cache: str,
    environment: dict[str, str] | None,
) -> float:
    """One cold build of one arm; its wall-clock seconds, or [`ArmError`]."""
    child = dict(os.environ)
    child.pop("FBUILD_PREBUILT_CORE", None)
    child.pop("FBUILD_NO_PREBUILT_LIBS", None)
    child.update(environment or {})
    child["FBUILD_CACHE_DIR"] = cache
    command = [
        str(fbuild),
        "build",
        str(project),
        "-e",
        env_name,
        "--no-timestamp",
        flag,
    ]
    started = time.monotonic()
    completed = subprocess.run(
        command,
        cwd=project,
        capture_output=True,
        text=True,
        check=False,
        env=child,
    )
    elapsed = time.monotonic() - started
    if completed.returncode != 0:
        tail = (completed.stderr or completed.stdout).strip().splitlines()[-3:]
        raise ArmError(
            f"the {arm} arm exited {completed.returncode}: {' | '.join(tail)}"
        )
    return round(elapsed, 3)


def read_prebuilt_record(project: Path, env_name: str, arm: str) -> dict[str, object]:
    """The `prebuilt_libs` block of this build's `build_info_<env>.json`."""
    path = project / f"build_info_{env_name}.json"
    try:
        outer = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ArmError(
            f"the {arm} arm wrote no readable {path.name}: {error}"
        ) from error
    document = parse_object_or_empty(outer, path.name)
    inner = parse_object_or_empty(document.get(env_name), f"{path.name}[{env_name}]")
    record = inner.get("prebuilt_libs")
    if not isinstance(record, dict):
        raise ArmError(f"the {arm} arm's {path.name} has no prebuilt_libs block")
    return {str(key): entry for key, entry in record.items()}


def parse_object_or_empty(value: object, what: str) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ArmError(f"{what} is not a JSON object")
    return {str(key): entry for key, entry in value.items()}


def enforce_arm(name: str, record: dict[str, object]) -> None:
    """A served arm that fell back is a hard failure, not a 1.00x row."""
    used = record.get("used")
    if name == "served" and used is not True:
        reason = record.get("reason")
        raise ArmError(
            "the served arm fell back to source "
            f"({reason or 'no reason recorded'}); a fallback is not a measurement "
            "(#1882 decision 6)"
        )
    if name == "source" and used is not False:
        raise ArmError(
            "the control arm reports a served artifact; --no-prebuilt-libs did not hold"
        )


def write_report(out_dir: Path, report: Report) -> Path:
    """Write `<YYYY-MM-DD>.json` under `out_dir`, atomically."""
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / f"{report.generated_at[:DATE_FORMAT_LENGTH]}.json"
    staging = path.with_suffix(".json.tmp")
    staging.write_text(json.dumps(report.as_json(), indent=2) + "\n", encoding="utf-8")
    staging.replace(path)
    return path


def merge_latest(out_dir: Path, report: Report) -> Report:
    """Keep `latest.json` on the NEWEST run; an older report never replaces it.

    Two overlapping benchmark runs publish into the same directory (the same
    overlap `ci/publish_bench_site.py` handles for the site overlay), so a run
    that finishes last must not hand the site an older measurement.
    """
    path = out_dir / "latest.json"
    current: Report | None = None
    if path.exists():
        try:
            current = load_report(path)
        except ReportError:
            current = None
    kept = report
    if current is not None and current.generated_at > report.generated_at:
        kept = current
    if kept is report:
        out_dir.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(report.as_json(), indent=2) + "\n", encoding="utf-8")
    return kept


def gate(
    report: Report,
    baseline: Report | None,
    *,
    reference: Cell = REFERENCE_CELL,
    budget: float = REGRESSION_BUDGET,
) -> dict[str, object]:
    """Decision 6's regression gate for one fresh measurement."""
    verdict: dict[str, object] = {
        "verdict": "pass",
        "reference_cell": report.cell == reference,
        "budget": budget,
        "baseline_s": None,
        "change": None,
        "reason": "not the reference cell; reported, never gated",
    }
    if report.cell != reference:
        return verdict
    if baseline is None or baseline.cell != reference:
        verdict["reason"] = "no reference-cell baseline yet"
        return verdict
    previous = baseline.served.p50_s
    current = report.served.p50_s
    change = (current - previous) / previous
    verdict["baseline_s"] = round(previous, 3)
    verdict["change"] = round(change, 6)
    if change > budget:
        verdict["verdict"] = "fail"
        verdict["reason"] = (
            f"cold_first_compile_wall_s regressed {change * 100:.1f}% "
            f"({previous:.1f}s -> {current:.1f}s) on the reference cell; "
            f"the budget is {budget * 100:.0f}%"
        )
    else:
        verdict["reason"] = (
            f"within the {budget * 100:.0f}% budget ({change * 100:+.1f}%)"
        )
    return verdict


def measurement_files(out_dir: Path) -> list[Path]:
    """Every dated measurement document in `out_dir`, newest last."""
    if not out_dir.is_dir():
        return []
    dated = [
        path
        for path in out_dir.glob("*.json")
        if path.name != "latest.json" and len(path.stem) == DATE_FORMAT_LENGTH
    ]
    return sorted(dated, key=lambda path: path.stem)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="The artifact-serving cold-build benchmark: two arms, one report, one gate."
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument(
        "--measure", action="store_true", help="run both arms and write a report"
    )
    mode.add_argument(
        "--check", action="store_true", help="gate the newest run against latest"
    )
    parser.add_argument(
        "--measurements", type=Path, help="measurements directory (--check)"
    )
    parser.add_argument("--fail", action="store_true", help="exit 3 on a gate failure")
    parser.add_argument(
        "--fbuild", type=Path, default=Path("fbuild"), help="fbuild binary"
    )
    parser.add_argument("--project", type=Path, help="the reference-cell project")
    parser.add_argument(
        "--env", default="esp32s3", help="PlatformIO environment to build"
    )
    parser.add_argument("--trials", type=int, default=3, help="cold trials per arm")
    parser.add_argument("--out", type=Path, help="measurements directory (--measure)")
    parser.add_argument("--sha", default=None, help="commit the measurement belongs to")
    parser.add_argument("--chip", default=REFERENCE_CELL_CHIP)
    parser.add_argument("--platform-version", default=REFERENCE_PLATFORM_VERSION)
    parser.add_argument("--os", default=REFERENCE_OS)
    arguments = parser.parse_args(argv)

    if arguments.check:
        return check(arguments.measurements, fail=arguments.fail)
    if arguments.project is None or arguments.out is None:
        parser.error("--measure needs --project and --out")
    cell = Cell(
        chip=arguments.chip,
        platform_version=arguments.platform_version,
        os=arguments.os,
    )
    try:
        report = measure(
            fbuild=arguments.fbuild,
            project=arguments.project,
            env_name=arguments.env,
            cell=cell,
            trials=arguments.trials,
            out_dir=arguments.out,
            sha=arguments.sha,
        )
    except ArmError as error:
        print(f"artifact-serving bench: {error}", file=sys.stderr)
        return 1
    path = write_report(arguments.out, report)
    merge_latest(arguments.out, report)
    metrics = report.metrics()
    print(
        f"artifact-serving bench: {cell.chip} served {metrics['cold_first_compile_wall_s']}s "
        f"vs source {metrics['cold_first_compile_wall_s_source']}s "
        f"= {metrics['speedup_ratio']}x -> {path}"
    )
    return 0


def check(measurements: Path | None, *, fail: bool) -> int:
    """Gate the newest dated report against `latest.json` in the same directory."""
    if measurements is None:
        print("artifact-serving bench: --check needs --measurements", file=sys.stderr)
        return 1
    candidates = measurement_files(measurements)
    if not candidates:
        print(f"artifact-serving bench: no reports in {measurements}; nothing to gate")
        return 0
    try:
        report = load_report(candidates[-1])
        latest_path = measurements / "latest.json"
        baseline = load_report(latest_path) if latest_path.exists() else None
    except ReportError as error:
        print(f"artifact-serving bench: {error}", file=sys.stderr)
        return 1
    verdict = gate(report, baseline)
    print(json.dumps(verdict, indent=2))
    if verdict["verdict"] == "fail":
        print(f"artifact-serving bench: FAIL - {verdict['reason']}", file=sys.stderr)
        return 3 if fail else 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

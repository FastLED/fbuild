"""Record the Dylint local-gate cold/warm measurement from existing Bosn runs.

FastLED/fbuild#1643 closed its parity half in #1646; the measurement half is
still open and nothing in this repository records it. A Bosn run record already
carries every field that issue asks to record -- source and tree identity, the
pinned engine and runner digests, the executed steps with their per-step
``duration_ms``, and (in the run's log stream) the snapshot and cache
provenance. So this module only reads and attributes. It never runs the gate,
and it never turns a run it did not see into a number.

What a pair of records can prove, and what it cannot:

* ``identical-input reuse`` -- equal ``tree_digest`` and equal ``payload_sha256``
* ``fresh-engine warm``     -- equal ``tree_digest``, different ``engine_id``
* ``changed-source warm``   -- different ``tree_digest``, same ``engine_id``
* ``cold``                  -- NEVER provable from a Bosn record: the record
  has no cache-state field, so a first run is reported as unproven rather than
  inferred from elapsed time.

A run that did not complete successfully is still rendered (it is usually the
interesting one), but it is marked ``evidence: no`` and is never paired with
anything: an incomplete, skipped or failed execution must not become a passing
measurement.

Bosn records also carry no host-load field. A controlled measurement needs an
idle host, so the report repeats that caveat instead of implying control.

Usage::

    uv run --no-project python -m ci.measure_dylint_gate RUN [RUN ...]
    uv run --no-project python -m ci.measure_dylint_gate --latest 4 --json
    uv run --no-project python -m ci.measure_dylint_gate --from-file run.json RUN

``--from-file`` reads a ``bosn ci show <run> --json`` document instead of
asking Bosn, which is what the tests use. Exit code is 0 when at least one run
was a complete success, 1 when none was.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
from dataclasses import dataclass, field
from pathlib import Path

from ci.local_gate import JsonValue, bosn_command, document, output

# Step names are classified by the job that ran them and the literal name the
# workflow gives them, because the aggregate stages (Setup/Pre/Post/Complete)
# repeat the same names inside every job and would otherwise be counted twice.
CHECKOUT_PREFIX = "Checkout"
AGGREGATION_PREFIX = "Require "
SETUP_PREFIXES = (
    "Set up",
    "Setup",
    "Install",
    "Prepare",
    "Provision",
    "Repair",
    "Select ",
)
OVERHEAD_STAGES = frozenset({"Setup", "Pre", "Post", "Complete"})
FRAME_STAGES = frozenset({"Main"})

# Log lines that carry cache or payload provenance. Counted and sampled, never
# used as a verdict: the sampled text is the evidence, the count is a pointer.
PROVENANCE_KEYWORDS = ("snapshot", "cache", "restore", "saved", "reuse", "hit", "miss")
PROVENANCE_SAMPLES = 2


@dataclass(frozen=True)
class StepDuration:
    """One executed (or explicitly skipped) workflow step of one job."""

    job: str
    name: str
    stage: str
    status: str
    conclusion: str | None
    duration_ms: int | None

    @property
    def executed(self) -> bool:
        return self.stage in FRAME_STAGES and self.conclusion == "success"


@dataclass(frozen=True)
class Attribution:
    """Per-step durations folded into the overheads #1643 names."""

    checkout_ms: int = 0
    setup_ms: int = 0
    cache_ms: int = 0
    work_ms: int = 0
    aggregation_ms: int = 0
    overhead_ms: int = 0

    @property
    def attributed_ms(self) -> int:
        return (
            self.checkout_ms
            + self.setup_ms
            + self.cache_ms
            + self.work_ms
            + self.aggregation_ms
            + self.overhead_ms
        )


@dataclass(frozen=True)
class RunMeasurement:
    """Everything one Bosn run record states about itself."""

    run_id: str
    sha: str
    git_tree: str
    tree_digest: str
    payload_sha256: str
    engine_id: str
    act_version: str
    act_binary_digest: str
    runner_image: str
    workflow: str
    event: str
    trigger: str
    mode: str
    conclusion: str
    exit_code: int
    started_at: float
    wall_ms: int
    steps: tuple[StepDuration, ...]
    attribution: Attribution
    provenance: dict[str, int] = field(default_factory=dict)
    samples: dict[str, tuple[str, ...]] = field(default_factory=dict)

    @property
    def executed_steps(self) -> tuple[StepDuration, ...]:
        return tuple(step for step in self.steps if step.executed)

    @property
    def complete(self) -> bool:
        """A success that actually executed work, not an all-skipped shell."""
        return (
            self.conclusion == "success"
            and self.exit_code == 0
            and bool(self.executed_steps)
        )

    @property
    def serial_gap_ms(self) -> int:
        """Wall clock minus the serial step sum.

        Negative means jobs overlapped: the sum adds every job's steps, so a
        run with concurrent jobs spends more step time than wall time. That is
        the normal shape and is why this is reported as a gap, not an overhead.
        """
        return self.wall_ms - self.attribution.attributed_ms

    @property
    def closure(self) -> tuple[str, str, str, str]:
        return (self.workflow, self.event, self.trigger, self.mode)


@dataclass(frozen=True)
class ReusePair:
    """Two complete runs of the same workflow closure, earlier then later."""

    earlier: str
    later: str
    regime: str
    earlier_ms: int
    later_ms: int

    @property
    def delta_ms(self) -> int:
        return self.later_ms - self.earlier_ms


def _string(raw: dict[str, JsonValue], key: str) -> str:
    value = raw.get(key)
    if not isinstance(value, str) or not value:
        raise ValueError(f"bosn run record lacks non-empty field {key}")
    return value


def _count(raw: dict[str, JsonValue], key: str) -> int:
    value = raw.get(key)
    if type(value) is not int or value < 0:
        raise ValueError(f"bosn run record lacks nonnegative integer field {key}")
    return value


def _optional_count(raw: dict[str, JsonValue], key: str) -> int | None:
    value = raw.get(key)
    if value is None:
        return None
    if type(value) is not int or value < 0:
        raise ValueError(f"bosn run record has an invalid {key}")
    return value


def _epoch(value: JsonValue, key: str) -> float:
    """Bosn stamps are epoch seconds; section stamps are RFC 3339."""
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return float(value)
    if isinstance(value, str) and value:
        return _dt.datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    raise ValueError(f"bosn run record lacks a usable {key} stamp")


def _pinned(raw: dict[str, JsonValue], key: str) -> str:
    pins = raw.get("execution_pins")
    if not isinstance(pins, dict):
        raise ValueError("bosn run record lacks execution pins")
    value = pins.get(key)
    if not isinstance(value, str) or not value:
        raise ValueError(f"bosn execution pins lack {key}")
    return value


def read_steps(raw: dict[str, JsonValue]) -> tuple[StepDuration, ...]:
    """Every step of every job, including the aggregate stages."""
    tree = raw.get("tree")
    if not isinstance(tree, dict) or not isinstance(groups := tree.get("groups"), list):
        raise ValueError("bosn run record lacks job groups")
    steps: list[StepDuration] = []
    for group in groups:
        if not isinstance(group, dict) or not isinstance(
            jobs := group.get("jobs"), list
        ):
            raise ValueError("invalid bosn job group")
        for job in jobs:
            if not isinstance(job, dict):
                raise ValueError("invalid bosn job record")
            job_id = _string(job, "job_id")
            if not isinstance(sections := job.get("sections"), list):
                raise ValueError("bosn job record lacks step evidence")
            for section in sections:
                if not isinstance(section, dict):
                    raise ValueError("invalid bosn step record")
                conclusion = section.get("conclusion")
                if conclusion is not None and not isinstance(conclusion, str):
                    raise ValueError("invalid bosn step conclusion")
                steps.append(
                    StepDuration(
                        job_id,
                        _string(section, "name"),
                        _string(section, "stage"),
                        _string(section, "status"),
                        conclusion,
                        _optional_count(section, "duration_ms"),
                    )
                )
    return tuple(steps)


def attribute(steps: tuple[StepDuration, ...]) -> Attribution:
    """Fold step durations into the overheads #1643 asks to separate."""
    totals = {name: 0 for name in Attribution.__dataclass_fields__}
    for step in steps:
        if step.duration_ms is None:
            continue
        totals[classify_step(step)] += step.duration_ms
    return Attribution(**totals)


def classify_step(step: StepDuration) -> str:
    """Name the attribution bucket of one step. Order is the contract."""
    if step.stage in OVERHEAD_STAGES:
        return "overhead_ms"
    if step.name.startswith(CHECKOUT_PREFIX):
        return "checkout_ms"
    if "cache" in step.name.lower():
        return "cache_ms"
    if step.name.startswith(AGGREGATION_PREFIX):
        return "aggregation_ms"
    if step.name.startswith(SETUP_PREFIXES):
        return "setup_ms"
    return "work_ms"


def read_provenance(
    lines: list[str],
) -> tuple[dict[str, int], dict[str, tuple[str, ...]]]:
    """Count and sample the cache/payload provenance lines of a log stream."""
    counts: dict[str, int] = {}
    samples: dict[str, list[str]] = {}
    for line in lines:
        lowered = line.lower()
        for keyword in PROVENANCE_KEYWORDS:
            if keyword not in lowered:
                continue
            counts[keyword] = counts.get(keyword, 0) + 1
            kept = samples.setdefault(keyword, [])
            if len(kept) < PROVENANCE_SAMPLES:
                kept.append(line.strip())
    return counts, {key: tuple(value) for key, value in samples.items()}


def measure(
    run_id: str,
    raw: dict[str, JsonValue],
    log_lines: list[str] | None = None,
) -> RunMeasurement:
    """Bind one Bosn run record to its identity, timing and provenance."""
    steps = read_steps(raw)
    counts, samples = read_provenance(log_lines or [])
    started = _epoch(raw.get("started_at"), "started_at")
    finished = _epoch(raw.get("finished_at"), "finished_at")
    return RunMeasurement(
        run_id=_string(raw, "id"),
        sha=_string(raw, "sha"),
        git_tree=_string(raw, "git_tree"),
        tree_digest=_string(raw, "tree_digest"),
        payload_sha256=_string(raw, "payload_sha256"),
        engine_id=_string(raw, "engine_id"),
        act_version=_string(raw, "act_version"),
        act_binary_digest=_pinned(raw, "act_binary_digest"),
        runner_image=_string(raw, "runner_image"),
        workflow=_string(raw, "workflow"),
        event=_string(raw, "event"),
        trigger=_string(raw, "trigger"),
        mode=_string(raw, "mode"),
        conclusion=_string(raw, "conclusion"),
        exit_code=_count(raw, "exit_code"),
        started_at=started,
        wall_ms=round((finished - started) * 1000),
        steps=steps,
        attribution=attribute(steps),
        provenance=counts,
        samples=samples,
    )


def classify_pair(earlier: RunMeasurement, later: RunMeasurement) -> str:
    """Name what two complete runs of one closure differ in. Never infers."""
    same_input = (
        earlier.tree_digest == later.tree_digest
        and earlier.payload_sha256 == later.payload_sha256
    )
    same_tree = earlier.tree_digest == later.tree_digest
    same_engine = earlier.engine_id == later.engine_id
    if same_input:
        return "identical-input reuse" if same_engine else "fresh-engine warm"
    if same_tree:
        return "identical-source, new payload" + (
            "" if same_engine else ", fresh engine"
        )
    if same_engine:
        return "changed-source warm"
    return "changed-source, fresh engine"


def pairs(runs: tuple[RunMeasurement, ...]) -> tuple[ReusePair, ...]:
    """Every earlier/later pair of complete runs in one workflow closure."""
    complete_runs = [run for run in runs if run.complete]
    ordered = sorted(complete_runs, key=lambda run: run.started_at)
    found: list[ReusePair] = []
    for index, earlier in enumerate(ordered):
        for later in ordered[index + 1 :]:
            if earlier.closure != later.closure:
                continue
            found.append(
                ReusePair(
                    earlier.run_id,
                    later.run_id,
                    classify_pair(earlier, later),
                    earlier.wall_ms,
                    later.wall_ms,
                )
            )
    return tuple(found)


def demanded_regimes(found: tuple[ReusePair, ...]) -> dict[str, tuple[ReusePair, ...]]:
    """Which of #1643's named regimes this set of records proves."""
    covered: dict[str, tuple[ReusePair, ...]] = {
        "identical-input reuse": tuple(
            p for p in found if p.regime == "identical-input reuse"
        ),
        "fresh-engine warm": tuple(p for p in found if p.regime == "fresh-engine warm"),
        "changed-source warm": tuple(
            p for p in found if p.regime == "changed-source warm"
        ),
        "cold": (),
    }
    return covered


def render_text(runs: tuple[RunMeasurement, ...], found: tuple[ReusePair, ...]) -> str:
    """A human report: per-run attribution, then pairs, then the caveats."""
    lines = ["# Dylint local-gate measurement", ""]
    for run in runs:
        attr = run.attribution
        lines.extend(
            (
                f"## {run.run_id[:8]}  {run.workflow}  {run.event}/{run.mode}",
                f"  evidence: {'yes' if run.complete else 'NO'} ({run.conclusion}, exit {run.exit_code})",
                f"  source:   sha {run.sha}  git_tree {run.git_tree}",
                f"  identity: tree {run.tree_digest}  payload {run.payload_sha256}",
                f"  engine:   {run.engine_id}  act {run.act_version}  {run.act_binary_digest}",
                f"  runner:   {run.runner_image}",
                f"  wall:     {run.wall_ms} ms  (serial step sum {run.attribution.attributed_ms} ms,"
                f" gap {run.serial_gap_ms} ms)",
                f"  overheads: checkout {attr.checkout_ms}  setup {attr.setup_ms}"
                f"  work {attr.work_ms}  cache {attr.cache_ms}"
                f"  aggregation {attr.aggregation_ms}  frame {attr.overhead_ms}",
                f"  executed: {len(run.executed_steps)} step(s)",
            )
        )
        for step in run.executed_steps:
            lines.append(
                f"    - {step.job}/{step.name} ({step.stage}) {step.duration_ms} ms"
            )
        if run.provenance:
            listed = ", ".join(
                f"{key}={value}" for key, value in run.provenance.items()
            )
            lines.append(f"  provenance (sampled log lines): {listed}")
        lines.append("")

    lines.append("## pairs (complete runs, same workflow closure)")
    if not found:
        lines.append("  none: no two complete runs share a workflow closure")
    for pair in found:
        lines.append(
            f"  {pair.regime}: {pair.earlier[:8]} -> {pair.later[:8]}"
            f"  {pair.earlier_ms} -> {pair.later_ms} ms ({pair.delta_ms:+d})"
        )
    lines.extend(("", "## regime coverage"))
    for regime, covering in demanded_regimes(found).items():
        if covering:
            lines.append(f"  {regime}: PROVEN by {len(covering)} pair(s)")
        elif regime == "cold":
            lines.append(
                "  cold: NOT PROVABLE from a bosn record (no cache-state field);"
                " record it from an idle host with a cleared cache instead"
            )
        else:
            lines.append(f"  {regime}: unproven in this set")
    lines.extend(
        (
            "",
            "Bosn records carry no host-load field: a controlled measurement needs an",
            "idle host, and this report does not claim one.",
            "",
        )
    )
    return "\n".join(lines)


def _read_log_lines(run_id: str) -> list[str]:
    """The run's log stream, one record per line, as text for provenance."""
    text = output(bosn_command("ci", "logs", run_id, "--json"))
    lines: list[str] = []
    for raw in text.splitlines():
        if not raw.strip():
            continue
        record: JsonValue = json.loads(raw)
        if not isinstance(record, dict):
            continue
        text = record.get("text")
        if isinstance(text, str):
            lines.append(text)
    return lines


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runs", nargs="*", help="bosn run ids")
    parser.add_argument("--latest", type=int, help="measure the newest N runs")
    parser.add_argument(
        "--from-file",
        action="append",
        default=[],
        metavar="PATH",
        help="read a `bosn ci show RUN --json` document instead of asking bosn",
    )
    parser.add_argument("--json", action="store_true", help="emit the record as json")
    parser.add_argument("--no-logs", action="store_true", help="skip log provenance")
    args = parser.parse_args()

    measured: list[RunMeasurement] = []
    for path in args.from_file:
        payload: JsonValue = json.loads(Path(path).read_text(encoding="utf-8"))
        if not isinstance(payload, dict):
            raise ValueError(f"{path} is not a bosn run record")
        measured.append(measure(_string(payload, "id"), payload))
    for run_id in args.runs:
        measured.append(
            measure(run_id, document(bosn_command("ci", "show", run_id, "--json")))
        )
    if args.latest:
        for run_id in _latest_ids(args.latest):
            measured.append(
                measure(run_id, document(bosn_command("ci", "show", run_id, "--json")))
            )
    if not measured:
        parser.error("name a run id, --latest N, or --from-file PATH")

    runs = tuple(measured)
    found = pairs(runs)
    if args.json:
        print(json.dumps(_as_json(runs, found), indent=2))
    else:
        print(render_text(runs, found))
    if not any(run.complete for run in runs):
        raise SystemExit(1)


def _latest_ids(count: int) -> list[str]:
    listing = document(bosn_command("ci", "list", "--limit", str(count), "--json"))
    runs = listing.get("runs")
    if not isinstance(runs, list):
        raise ValueError("bosn run listing lacks a runs array")
    return [_string(run, "id") for run in runs if isinstance(run, dict)]


def _run_json(run: RunMeasurement) -> JsonValue:
    """One run's record, in the order a reader wants it."""
    attribution: dict[str, JsonValue] = {}
    for name in Attribution.__dataclass_fields__:
        attribution[name.removesuffix("_ms")] = getattr(run.attribution, name)
    steps: list[JsonValue] = []
    for step in run.executed_steps:
        steps.append(
            {"job": step.job, "name": step.name, "duration_ms": step.duration_ms}
        )
    provenance: dict[str, JsonValue] = {}
    for key, count in run.provenance.items():
        provenance[key] = count
    samples: dict[str, JsonValue] = {}
    for key, sample in run.samples.items():
        samples[key] = list(sample)
    return {
        "id": run.run_id,
        "evidence": run.complete,
        "sha": run.sha,
        "git_tree": run.git_tree,
        "tree_digest": run.tree_digest,
        "payload_sha256": run.payload_sha256,
        "engine_id": run.engine_id,
        "act_version": run.act_version,
        "act_binary_digest": run.act_binary_digest,
        "runner_image": run.runner_image,
        "workflow": run.workflow,
        "event": run.event,
        "trigger": run.trigger,
        "mode": run.mode,
        "conclusion": run.conclusion,
        "exit_code": run.exit_code,
        "started_at": run.started_at,
        "wall_ms": run.wall_ms,
        "serial_gap_ms": run.serial_gap_ms,
        "attribution_ms": attribution,
        "executed_steps": steps,
        "provenance": provenance,
        "provenance_samples": samples,
    }


def _as_json(
    runs: tuple[RunMeasurement, ...], found: tuple[ReusePair, ...]
) -> dict[str, JsonValue]:
    runs_json: list[JsonValue] = [_run_json(run) for run in runs]
    pairs_json: list[JsonValue] = []
    for pair in found:
        pairs_json.append(
            {
                "earlier": pair.earlier,
                "later": pair.later,
                "regime": pair.regime,
                "earlier_ms": pair.earlier_ms,
                "later_ms": pair.later_ms,
                "delta_ms": pair.delta_ms,
            }
        )
    regimes_json: dict[str, JsonValue] = {}
    for regime, covering in demanded_regimes(found).items():
        regimes_json[regime] = [f"{pair.earlier}->{pair.later}" for pair in covering]
    return {
        "runs": runs_json,
        "pairs": pairs_json,
        "regimes": regimes_json,
        "caveats": [
            "cold is not provable from a bosn record (no cache-state field)",
            "bosn records carry no host-load field; a controlled measurement needs an idle host",
        ],
    }


if __name__ == "__main__":
    main()

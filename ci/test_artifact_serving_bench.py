"""The artifact-serving bench must measure two distinct arms, or say nothing.

Decision 6 of `docs/design/artifact-serving-decisions.md` asks for one headline
number *with its control arm*: `cold_first_compile_wall_s` with prebuilt libs and
`cold_first_compile_wall_s_source` from a `--no-prebuilt-libs` run in the same
job, plus a gate that fails a regression above 10% on the reference cell.

A ratio computed from two runs that both compiled from source reads exactly like
a 1.00x speedup, and nothing downstream can tell the difference -- decision 6's
whole point is that a served arm which silently fell back is *not* a
measurement. These tests drive the real seams: a real `fbuild` shim that writes
the real `build_info_<env>.json` shape (`{ "<env>": { "prebuilt_libs": ... } }`,
compare `crates/fbuild-build-engine/src/build_info.rs`), real trial caches, real
JSON files.
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

from ci import artifact_serving_bench as bench


def mapping(value: object) -> dict[str, object]:
    """`value` as a JSON object, so a test reads fields instead of types."""
    if not isinstance(value, dict):
        raise AssertionError(f"not a JSON object: {value!r}")
    return {str(key): entry for key, entry in value.items()}


def number(value: object) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise AssertionError(f"not a number: {value!r}")
    return float(value)


def trials_of(arms: dict[str, object], name: str) -> list[object]:
    trials = mapping(arms[name])["wall_s"]
    if not isinstance(trials, list):
        raise AssertionError(f"arms.{name}.wall_s is not a list")
    return trials


def argv_of(entry: dict[str, object]) -> list[object]:
    argv = entry["argv"]
    if not isinstance(argv, list):
        raise AssertionError(f"argv is not a list: {argv!r}")
    return argv


SHIM = """#!{python}
import json, os, sys, time

args = sys.argv[1:]
log = os.environ.get("SHIM_LOG")
if log:
    with open(log, "a", encoding="utf-8") as handle:
        handle.write(json.dumps({{"argv": args, "cache": os.environ.get("FBUILD_CACHE_DIR")}}) + "\\n")

served = "--prebuilt-core" in args
if os.environ.get("SHIM_FORCE_SOURCE") == "1":
    served = False
time.sleep(float(os.environ.get("SHIM_SLEEP", "0.01")))

project = args[1]
env_name = args[args.index("-e") + 1]
if served:
    record = {{
        "used": True,
        "source": "https://example.invalid/core.tar.zst",
        "key": "arduino-esp32/3.3.12/esp32s3/fp-0123456789abcdef",
        "sha256": "0123456789abcdef",
        "fingerprint": "0123456789abcdef" * 4,
        "reason": None,
    }}
else:
    record = {{
        "used": False,
        "source": "source",
        "key": None,
        "sha256": None,
        "fingerprint": "0123456789abcdef" * 4,
        "reason": "prebuilt core: no published tree for this identity",
    }}
path = os.path.join(project, "build_info_%s.json" % env_name)
with open(path, "w", encoding="utf-8") as handle:
    json.dump({{env_name: {{"prebuilt_libs": record}}}}, handle, indent=2)
raise SystemExit(int(os.environ.get("SHIM_STATUS", "0")))
"""


class Fixture:
    """A throwaway project plus an executable `fbuild` shim that records argv."""

    def __init__(self, tmp: Path) -> None:
        self.root = tmp
        self.project = tmp / "project"
        self.project.mkdir()
        self.log = tmp / "shim.jsonl"
        self.fbuild = tmp / "fbuild"
        self.fbuild.write_text(SHIM.format(python=sys.executable), encoding="utf-8")
        self.fbuild.chmod(0o755)
        self.out = tmp / "measurements"
        self.out.mkdir()

    def environment(self) -> dict[str, str]:
        return {"SHIM_LOG": str(self.log)}

    def invocations(self) -> list[dict[str, object]]:
        if not self.log.exists():
            return []
        return [
            json.loads(line)
            for line in self.log.read_text(encoding="utf-8").splitlines()
        ]

    def measure(self, trials: int = 2, cell: bench.Cell = bench.REFERENCE_CELL):
        return bench.measure(
            fbuild=self.fbuild,
            project=self.project,
            env_name="esp32s3",
            cell=cell,
            trials=trials,
            out_dir=self.out,
            environment=self.environment(),
            sleep_between_trials_s=0.0,
        )


class MeasureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.fixture = Fixture(Path(self.tmp.name))

    def test_a_report_carries_both_arms_their_p50s_and_the_ratio(self) -> None:
        report = self.fixture.measure(trials=3)
        document = report.as_json()
        arms = mapping(document["arms"])
        self.assertEqual(document["schema"], bench.SCHEMA)
        self.assertEqual(document["trials"], 3)
        self.assertEqual(len(trials_of(arms, "served")), 3)
        self.assertEqual(len(trials_of(arms, "source")), 3)
        metrics = mapping(document["metrics"])
        served = number(metrics["cold_first_compile_wall_s"])
        source = number(metrics["cold_first_compile_wall_s_source"])
        self.assertEqual(served, report.served.p50_s)
        self.assertGreater(source, 0.0)
        self.assertAlmostEqual(
            number(metrics["speedup_ratio"]), source / served, places=3
        )

    def test_each_trial_gets_its_own_empty_cache_root(self) -> None:
        self.fixture.measure(trials=2)
        caches = [str(entry["cache"]) for entry in self.fixture.invocations()]
        self.assertEqual(len(caches), 4)
        self.assertEqual(len(set(caches)), 4)
        for cache in caches:
            self.assertTrue(cache)

    def test_the_control_arm_passes_the_public_opt_out_and_the_served_arm_opt_in(
        self,
    ) -> None:
        self.fixture.measure(trials=1)
        commands = [
            " ".join(str(part) for part in argv_of(entry))
            for entry in self.fixture.invocations()
        ]
        served = [argv for argv in commands if "--prebuilt-core" in argv]
        source = [argv for argv in commands if "--no-prebuilt-libs" in argv]
        self.assertEqual(len(served), 1)
        self.assertEqual(len(source), 1)
        self.assertNotIn("--prebuilt-core", source[0])

    def test_a_served_arm_that_fell_back_to_source_is_not_a_measurement(self) -> None:
        environment = self.fixture.environment() | {"SHIM_FORCE_SOURCE": "1"}
        with self.assertRaises(bench.ArmError) as caught:
            bench.measure(
                fbuild=self.fixture.fbuild,
                project=self.fixture.project,
                env_name="esp32s3",
                cell=bench.REFERENCE_CELL,
                trials=1,
                out_dir=self.fixture.out,
                environment=environment,
                sleep_between_trials_s=0.0,
            )
        self.assertIn("fell back", str(caught.exception))

    def test_a_failed_build_names_the_arm_and_the_status(self) -> None:
        environment = self.fixture.environment() | {"SHIM_STATUS": "7"}
        with self.assertRaises(bench.ArmError) as caught:
            bench.measure(
                fbuild=self.fixture.fbuild,
                project=self.fixture.project,
                env_name="esp32s3",
                cell=bench.REFERENCE_CELL,
                trials=1,
                out_dir=self.fixture.out,
                environment=environment,
                sleep_between_trials_s=0.0,
            )
        self.assertIn("served arm exited 7", str(caught.exception))

    def test_the_report_is_written_and_latest_follows_the_newest_run(self) -> None:
        report = self.fixture.measure(trials=1)
        path = bench.write_report(self.fixture.out, report)
        self.assertTrue(path.exists())
        self.assertEqual(bench.load_report(path).cell, bench.REFERENCE_CELL)
        latest = bench.merge_latest(self.fixture.out, bench.load_report(path))
        self.assertEqual(latest.cell, bench.REFERENCE_CELL)
        self.assertTrue((self.fixture.out / "latest.json").exists())


class GateTests(unittest.TestCase):
    def report(
        self, served_p50: float, cell: bench.Cell = bench.REFERENCE_CELL
    ) -> bench.Report:
        arm_served = bench.Arm(
            wall_s=(served_p50,), bytes_downloaded=None, prebuilt={"used": True}
        )
        arm_source = bench.Arm(
            wall_s=(served_p50 * 2.0,), bytes_downloaded=None, prebuilt={"used": False}
        )
        return bench.Report(
            generated_at="2026-11-28T15:00:00Z",
            sha="bc1c3d96",
            cell=cell,
            served=arm_served,
            source=arm_source,
        )

    def test_exactly_ten_percent_is_inside_the_budget(self) -> None:
        verdict = bench.gate(self.report(110.0), self.report(100.0))
        self.assertEqual(verdict["verdict"], "pass")
        self.assertAlmostEqual(number(verdict["change"]), 0.1, places=6)

    def test_more_than_ten_percent_fails_and_names_both_numbers(self) -> None:
        verdict = bench.gate(self.report(110.5), self.report(100.0))
        self.assertEqual(verdict["verdict"], "fail")
        self.assertIn("110.5", str(verdict["reason"]))
        self.assertIn("100.0", str(verdict["reason"]))

    def test_a_measurement_that_got_faster_passes(self) -> None:
        verdict = bench.gate(self.report(80.0), self.report(100.0))
        self.assertEqual(verdict["verdict"], "pass")

    def test_a_non_reference_cell_is_reported_but_never_gated(self) -> None:
        other = bench.Cell(
            chip="esp32dev", platform_version="core-3.3.12", os="ubuntu-22.04"
        )
        verdict = bench.gate(self.report(400.0, other), self.report(100.0))
        self.assertEqual(verdict["verdict"], "pass")
        self.assertFalse(verdict["reference_cell"])

    def test_the_first_run_has_no_baseline_and_passes(self) -> None:
        verdict = bench.gate(self.report(100.0), None)
        self.assertEqual(verdict["verdict"], "pass")
        self.assertIsNone(verdict["baseline_s"])


class ReportDocumentTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.fixture = Fixture(Path(self.tmp.name))
        self.directory = self.fixture.out
        self.report = self.fixture.measure(trials=1)
        self.path = bench.write_report(self.directory, self.report)

    def rewrite(self, mutate) -> Path:
        document = json.loads(self.path.read_text(encoding="utf-8"))
        mutate(document)
        target = self.directory / "broken.json"
        target.write_text(json.dumps(document), encoding="utf-8")
        return target

    def test_a_document_missing_an_arm_is_refused(self) -> None:
        orphan = self.rewrite(lambda document: document["arms"].pop("source"))
        with self.assertRaises(bench.ReportError):
            bench.load_report(orphan)

    def test_a_foreign_schema_is_refused(self) -> None:
        foreign = self.rewrite(lambda document: document.update({"schema": 2}))
        with self.assertRaises(bench.ReportError):
            bench.load_report(foreign)

    def test_an_empty_trial_list_is_refused(self) -> None:
        empty = self.rewrite(
            lambda document: document["arms"]["served"].update({"wall_s": []})
        )
        with self.assertRaises(bench.ReportError):
            bench.load_report(empty)

    def test_latest_never_goes_backwards_in_time(self) -> None:
        newer = self.report.as_json() | {"generated_at": "2026-11-29T00:00:00Z"}
        (self.directory / "newer.json").write_text(json.dumps(newer), encoding="utf-8")
        kept = bench.merge_latest(
            self.directory, bench.load_report(self.directory / "newer.json")
        )
        older = self.report.as_json() | {"generated_at": "2026-11-27T00:00:00Z"}
        (self.directory / "older.json").write_text(json.dumps(older), encoding="utf-8")
        kept_after = bench.merge_latest(
            self.directory, bench.load_report(self.directory / "older.json")
        )
        self.assertEqual(kept.generated_at, "2026-11-29T00:00:00Z")
        self.assertEqual(kept_after.generated_at, "2026-11-29T00:00:00Z")


class CommandTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.fixture = Fixture(Path(self.tmp.name))

    def test_check_gates_the_newest_run_against_latest(self) -> None:
        baseline = self.fixture.measure(trials=1)
        baseline = bench.Report(
            generated_at="2026-11-27T00:00:00Z",
            sha=baseline.sha,
            cell=baseline.cell,
            served=bench.Arm((100.0,), None, {"used": True}),
            source=baseline.source,
        )
        bench.merge_latest(self.fixture.out, baseline)
        served = bench.Arm((150.0,), None, {"used": True})
        fresh = bench.Report(
            generated_at="2026-11-28T00:00:00Z",
            sha="bc1c3d96",
            cell=bench.REFERENCE_CELL,
            served=served,
            source=bench.Arm((300.0,), None, {"used": False}),
        )
        bench.write_report(self.fixture.out, fresh)
        code = bench.main(
            ["--check", "--measurements", str(self.fixture.out), "--fail"]
        )
        self.assertEqual(code, 3)
        code = bench.main(["--check", "--measurements", str(self.fixture.out)])
        self.assertEqual(code, 0)

    def test_check_without_any_report_passes(self) -> None:
        self.assertEqual(
            bench.main(["--check", "--measurements", str(self.fixture.out)]), 0
        )

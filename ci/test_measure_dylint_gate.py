"""The measurement recorder reports only what a Bosn record proves."""

import unittest
from unittest.mock import patch

from ci import measure_dylint_gate as measure_module
from ci.local_gate import JsonValue
from ci.measure_dylint_gate import (
    Attribution,
    _as_json,
    demanded_regimes,
    measure,
    pairs,
    read_provenance,
    render_text,
)


def section(
    name: str,
    stage: str = "Main",
    conclusion: str | None = "success",
    duration_ms: int | None = 1000,
) -> dict[str, JsonValue]:
    return {
        "name": name,
        "stage": stage,
        "status": "completed",
        "conclusion": conclusion,
        "duration_ms": duration_ms,
    }


def dylint_job(extra: list[JsonValue] | None = None) -> dict[str, JsonValue]:
    sections: list[JsonValue] = [
        section("Set up job", "Setup", duration_ms=1000),
        section("Checkout source", duration_ms=6000),
        section("Set up uv", duration_ms=10000),
        section("Provision cold-build disk capacity", duration_ms=240),
        section("Setup soldr", duration_ms=79000),
        section("Run dylint over workspace", duration_ms=700000),
        section("Set up uv", "Post", duration_ms=200),
        section("Complete job", "Complete", duration_ms=6000),
    ]
    for item in extra or []:
        sections.append(item)
    return {"job_id": "dylint", "sections": sections}


def gate_job() -> dict[str, JsonValue]:
    return {
        "job_id": "gate",
        "sections": [
            section("Set up job", "Setup", duration_ms=500),
            section("Require successful Dylint jobs", duration_ms=300),
        ],
    }


def record(
    run_id: str = "run-a",
    tree: str = "tree-a",
    payload: str = "payload-a",
    engine: str = "engine-a",
    conclusion: str = "success",
    exit_code: int = 0,
    event: str = "workflow_dispatch",
    jobs: list[JsonValue] | None = None,
) -> dict[str, JsonValue]:
    return {
        "id": run_id,
        "sha": "a" * 40,
        "git_tree": "b" * 40,
        "tree_digest": tree,
        "payload_sha256": payload,
        "engine_id": engine,
        "act_version": "0.2.89-act2.36",
        "execution_pins": {"act_binary_digest": "sha256:" + "c" * 64},
        "runner_image": "img@sha256:" + "d" * 64,
        "workflow": ".github/workflows/dylint.yml",
        "event": event,
        "trigger": event,
        "mode": "minimal",
        "conclusion": conclusion,
        "exit_code": exit_code,
        "started_at": 1000.0,
        "finished_at": 2000.0,
        "tree": {"groups": [{"name": "0", "jobs": jobs or [dylint_job(), gate_job()]}]},
    }


class AttributionTests(unittest.TestCase):
    def test_attribution_separates_the_named_overheads(self) -> None:
        run = measure("run-a", record())
        self.assertEqual(
            run.attribution,
            Attribution(
                checkout_ms=6000,
                setup_ms=89240,
                cache_ms=0,
                work_ms=700000,
                aggregation_ms=300,
                overhead_ms=7700,
            ),
        )
        self.assertEqual(run.wall_ms, 1000000)
        self.assertEqual(run.serial_gap_ms, 196760)

    def test_skipped_step_is_not_executed_evidence(self) -> None:
        skipped = section(
            "Save matching guarded cache", conclusion="skipped", duration_ms=None
        )
        run = measure("run-a", record(jobs=[dylint_job([skipped]), gate_job()]))
        names = [step.name for step in run.executed_steps]
        self.assertNotIn("Save matching guarded cache", names)
        self.assertEqual(run.attribution.cache_ms, 0)
        self.assertTrue(run.complete)

    def test_a_run_that_executed_nothing_is_not_evidence(self) -> None:
        bare = {"job_id": "dylint", "sections": [section("Set up job", "Setup")]}
        run = measure("run-a", record(jobs=[bare]))
        self.assertFalse(run.complete)

    def test_identity_fields_are_bound_from_the_record(self) -> None:
        run = measure(
            "run-a", record(tree="tree-x", payload="payload-x", engine="engine-x")
        )
        self.assertEqual(run.tree_digest, "tree-x")
        self.assertEqual(run.payload_sha256, "payload-x")
        self.assertEqual(run.engine_id, "engine-x")
        self.assertEqual(run.act_binary_digest, "sha256:" + "c" * 64)
        self.assertEqual(run.runner_image, "img@sha256:" + "d" * 64)
        self.assertEqual(run.executed_steps[0].job, "dylint")


class PairTests(unittest.TestCase):
    def regimes(
        self, first: dict[str, JsonValue], second: dict[str, JsonValue]
    ) -> list[str]:
        found = pairs((measure("run-a", first), measure("run-b", second)))
        return [pair.regime for pair in found]

    def test_identical_input_pair_is_identical_input_reuse(self) -> None:
        self.assertEqual(self.regimes(record(), record()), ["identical-input reuse"])

    def test_same_tree_new_engine_is_fresh_engine_warm(self) -> None:
        self.assertEqual(
            self.regimes(record(), record(engine="engine-b")), ["fresh-engine warm"]
        )

    def test_different_tree_same_engine_is_changed_source_warm(self) -> None:
        self.assertEqual(
            self.regimes(record(), record(tree="tree-b", payload="payload-b")),
            ["changed-source warm"],
        )

    def test_same_tree_new_payload_names_the_payload_change(self) -> None:
        self.assertEqual(
            self.regimes(record(), record(payload="payload-b")),
            ["identical-source, new payload"],
        )

    def test_failed_run_is_never_paired(self) -> None:
        self.assertEqual(
            self.regimes(record(), record(conclusion="failure", exit_code=1)), []
        )

    def test_different_closure_is_never_paired(self) -> None:
        self.assertEqual(self.regimes(record(), record(event="pull_request")), [])

    def test_cold_is_reported_as_unproven_rather_than_guessed(self) -> None:
        runs = (measure("run-a", record()), measure("run-b", record()))
        coverage = demanded_regimes(pairs(runs))
        self.assertEqual(coverage["cold"], ())
        self.assertEqual(len(coverage["identical-input reuse"]), 1)
        self.assertIn("NOT PROVABLE", render_text(runs, pairs(runs)))


class ProvenanceTests(unittest.TestCase):
    def test_log_provenance_counts_and_samples_the_snapshot_lines(self) -> None:
        counts, samples = read_provenance(
            [
                "30 actions/checkout step(s) of this repository are served from the frozen snapshot",
                "cache hit for soldr toolchain",
                "a line that carries nothing of interest",
            ]
        )
        self.assertEqual(counts["snapshot"], 1)
        self.assertEqual(counts["cache"], 1)
        self.assertNotIn("nothing", counts)
        self.assertTrue(samples["snapshot"][0].startswith("30 actions/checkout"))


class MalformedRecordTests(unittest.TestCase):
    def test_missing_job_groups_is_rejected(self) -> None:
        raw = record()
        raw["tree"] = {"malformed_lines": 0}
        with self.assertRaises(ValueError):
            measure("run-a", raw)

    def test_missing_tree_digest_is_rejected(self) -> None:
        raw = record()
        raw["tree_digest"] = ""
        with self.assertRaises(ValueError):
            measure("run-a", raw)

    def test_missing_engine_pin_is_rejected(self) -> None:
        raw = record()
        raw["execution_pins"] = {}
        with self.assertRaises(ValueError):
            measure("run-a", raw)

    def test_missing_start_stamp_is_rejected(self) -> None:
        raw = record()
        del raw["finished_at"]
        with self.assertRaises(ValueError):
            measure("run-a", raw)


class JsonRecordTests(unittest.TestCase):
    def test_record_is_serializable_and_carries_the_demanded_identity(self) -> None:
        import json

        runs = (
            measure("run-a", record()),
            measure("run-b", record(run_id="run-b", engine="engine-b")),
        )
        document = _as_json(runs, pairs(runs))
        json.dumps(document)  # a record that cannot be serialized is not a record
        entries = document["runs"]
        assert isinstance(entries, list)
        first = entries[0]
        assert isinstance(first, dict)
        for key in (
            "tree_digest",
            "payload_sha256",
            "engine_id",
            "act_binary_digest",
            "runner_image",
            "attribution_ms",
            "executed_steps",
            "provenance",
        ):
            self.assertIn(key, first)
        regimes = document["regimes"]
        assert isinstance(regimes, dict)
        self.assertEqual(regimes["fresh-engine warm"], ["run-a->run-b"])
        self.assertEqual(regimes["cold"], [])


class LatestSelectionTests(unittest.TestCase):
    """`--latest` selects one workflow, never whatever happened to run last.

    `bosn ci list` returns the newest runs of every workflow (`callee.yml`,
    `integration.yml`, `ci.yml`, ...), so a bare list limit is not a filter:
    asking for four runs can return nothing from the Dylint closure while the
    report still claims to measure it.
    """

    def select(
        self, count: int, pages: list[dict[str, JsonValue]]
    ) -> tuple[list[str], list[str]]:
        requests: list[str] = []
        pending = list(pages)

        def fake_bosn(*args: str) -> list[str]:
            requests.append(args[args.index("--limit") + 1])
            return ["bosn", *args]

        def fake_document(argv: list[str]) -> dict[str, JsonValue]:
            self.assertEqual(argv[0], "bosn")
            return pending.pop(0)

        with (
            patch.object(measure_module, "bosn_command", fake_bosn),
            patch.object(measure_module, "document", fake_document),
        ):
            return measure_module._latest_ids(count), requests

    @staticmethod
    def listing(*entries: tuple[str, str]) -> dict[str, JsonValue]:
        return {
            "runs": [
                {"id": run_id, "workflow": workflow} for run_id, workflow in entries
            ]
        }

    @staticmethod
    def callee(count: int) -> list[tuple[str, str]]:
        return [
            (f"callee-{index}", ".github/workflows/callee.yml")
            for index in range(count)
        ]

    def test_latest_reads_only_the_dylint_workflow(self) -> None:
        selected, _ = self.select(
            3,
            [
                self.listing(
                    ("newest", ".github/workflows/callee.yml"),
                    ("dylint-a", ".github/workflows/dylint.yml"),
                    ("middle", ".github/workflows/ci.yml"),
                    ("dylint-b", ".github/workflows/dylint.yml"),
                    ("oldest", ".github/workflows/integration.yml"),
                )
            ],
        )
        self.assertEqual(selected, ["dylint-a", "dylint-b"])

    def test_latest_escalates_the_listing_past_unrelated_runs(self) -> None:
        first_page = self.listing(
            *self.callee(49), ("dylint-old", ".github/workflows/dylint.yml")
        )
        second_page = self.listing(
            *self.callee(57),
            ("dylint-a", ".github/workflows/dylint.yml"),
            ("dylint-b", ".github/workflows/dylint.yml"),
            ("dylint-c", ".github/workflows/dylint.yml"),
        )
        selected, requests = self.select(2, [first_page, second_page])
        self.assertEqual(selected, ["dylint-a", "dylint-b"])
        self.assertEqual(len(requests), 2)
        self.assertGreater(int(requests[1]), int(requests[0]))

    def test_latest_stops_when_the_listing_is_exhausted(self) -> None:
        selected, requests = self.select(
            4,
            [
                self.listing(
                    ("dylint-a", ".github/workflows/dylint.yml"),
                    ("callee", ".github/workflows/callee.yml"),
                    ("dylint-b", ".github/workflows/dylint.yml"),
                )
            ],
        )
        self.assertEqual(selected, ["dylint-a", "dylint-b"])
        self.assertEqual(len(requests), 1)

    def test_latest_rejects_a_non_positive_count(self) -> None:
        with self.assertRaises(ValueError):
            measure_module._latest_ids(0)


if __name__ == "__main__":
    unittest.main()

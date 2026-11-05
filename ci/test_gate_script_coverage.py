"""Tests for ci/gate_script_coverage.py -- the gate-script test ratchet.

The registry replaces "remember to add a test when you add a gate".  These tests
drive the real seams against synthetic trees so every face of the ratchet fails
loudly: an unregistered gate, a declared test that is missing or no longer
mentions its subject, an entry nobody invokes, and -- the important one -- a
growing `untested` list.  One case then asserts the actual repository satisfies
the same rules.
"""

from __future__ import annotations

import contextlib
import io
import tempfile
import unittest
from pathlib import Path

from ci import gate_script_coverage as coverage

ROOT = Path(__file__).resolve().parents[1]

# The measured gap on 2026-11-05 was eight workflow-invoked gates with no
# test; `audit_ignored_tests`, `check_fbuild_path_baseline`,
# `check_flash_offsets` and `cancel_superseded` gained theirs, so the budget is
# four. Lowering this number is the work; raising it means the class regrew.
MAX_UNTESTED = 4

WORKFLOW = """\
jobs:
  gate:
    steps:
      - run: uv run --no-project python ci/{stem}.py --fail
"""


class Fixture:
    """A throwaway repository: one workflow, one gate, one optional test."""

    def __init__(self, tmp: Path) -> None:
        self.root = tmp
        self.workflows = tmp / ".github" / "workflows"
        self.workflows.mkdir(parents=True)
        self.ci = tmp / "ci"
        self.ci.mkdir()

    def gate(self, stem: str, invocation: str | None = None) -> None:
        if invocation is None:
            (self.workflows / f"{stem}.yml").write_text(
                WORKFLOW.format(stem=stem), encoding="utf-8"
            )
        else:
            (self.workflows / "ci-test.yml").write_text(invocation, encoding="utf-8")
        (self.ci / f"{stem}.py").write_text('"""A gate."""\n', encoding="utf-8")

    def covering_test(self, stem: str, mentions: str | None = None) -> str:
        relative = f"ci/test_{stem}.py"
        (self.root / relative).write_text(
            f'"""Tests for {mentions or stem}."""\n', encoding="utf-8"
        )
        return relative

    def registry(self, scripts: dict[str, list[str]], untested: list[str], schema: int = 1) -> Path:
        lines = [f"schema = {schema}", "untested = ["]
        lines += [f'  "{stem}",' for stem in untested]
        lines.append("]")
        for stem, tests in scripts.items():
            rendered = ", ".join(f'"{test}"' for test in tests)
            lines.append(f"[scripts.{stem}]")
            lines.append(f"tests = [{rendered}]")
        path = self.root / "registry.toml"
        path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return path


class GateCoverageCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="gate-coverage-")
        self.addCleanup(self._tmp.cleanup)
        self.fixture = Fixture(Path(self._tmp.name))

    def problems(self, registry, budget: int = 0) -> list[str]:
        return coverage.violations(self.fixture.root, registry, budget)

    def test_inventory_finds_both_invocation_spellings(self) -> None:
        self.fixture.gate("alpha")
        self.fixture.gate("beta", invocation="run: python -m ci.beta --fail\n")
        self.assertEqual(
            coverage.invoked_modules(self.fixture.root),
            {"alpha": ("alpha.yml",), "beta": ("ci-test.yml",)},
        )

    def test_unregistered_gate_is_a_violation(self) -> None:
        self.fixture.gate("alpha")
        registry = coverage.Registry(scripts={}, untested=())
        self.assertEqual(
            self.problems(registry),
            ["alpha: invoked by alpha.yml but absent from the registry"],
        )

    def test_registry_entry_nobody_invokes_is_a_violation(self) -> None:
        registry = coverage.Registry(scripts={"ghost": ("ci/test_ghost.py",)}, untested=())
        self.assertIn("ghost: declared but no workflow invokes it", self.problems(registry))

    def test_declared_test_must_exist(self) -> None:
        self.fixture.gate("alpha")
        registry = coverage.Registry(scripts={"alpha": ("ci/test_alpha.py",)}, untested=())
        self.assertIn("does not exist", self.problems(registry)[0])

    def test_declared_test_must_mention_its_subject(self) -> None:
        self.fixture.gate("alpha")
        relative = self.fixture.covering_test("alpha", mentions="something else")
        registry = coverage.Registry(scripts={"alpha": (relative,)}, untested=())
        self.assertEqual(
            self.problems(registry), [f"alpha: declared test {relative} never mentions it"]
        )

    def test_covered_gate_with_a_real_test_passes(self) -> None:
        self.fixture.gate("alpha")
        relative = self.fixture.covering_test("alpha")
        registry = coverage.Registry(scripts={"alpha": (relative,)}, untested=())
        self.assertEqual(self.problems(registry), [])

    def test_untested_gate_is_accepted_only_within_budget(self) -> None:
        self.fixture.gate("alpha")
        registry = coverage.Registry(scripts={"alpha": ()}, untested=("alpha",))
        self.assertEqual(self.problems(registry, budget=1), [])
        self.assertEqual(len(self.problems(registry, budget=0)), 1)

    def test_growing_the_untested_list_is_what_the_budget_blocks(self) -> None:
        for stem in ("alpha", "beta", "gamma"):
            self.fixture.gate(stem)
        registry = coverage.Registry(
            scripts={stem: () for stem in ("alpha", "beta", "gamma")},
            untested=("alpha", "beta", "gamma"),
        )
        self.assertEqual(self.problems(registry, budget=3), [])
        self.assertIn("have no test, budget is 1", self.problems(registry, budget=1)[0])

    def test_stale_untested_entry_fails_once_a_test_lands(self) -> None:
        self.fixture.gate("alpha")
        relative = self.fixture.covering_test("alpha")
        registry = coverage.Registry(scripts={"alpha": (relative,)}, untested=("alpha",))
        self.assertIn(
            f"alpha: listed as untested but declares tests ['{relative}']",
            self.problems(registry),
        )

    def test_untested_entry_without_a_declaration_fails(self) -> None:
        self.fixture.gate("alpha")
        registry = coverage.Registry(scripts={"alpha": ()}, untested=("alpha", "ghost"))
        self.assertIn(
            "ghost: listed as untested but not declared at all", self.problems(registry)
        )

    def test_test_modules_are_not_treated_as_gates(self) -> None:
        invocation = "run: python ci/test_cache_status.py\n"
        self.fixture.gate("test_cache_status", invocation=invocation)
        self.assertEqual(coverage.invoked_modules(self.fixture.root), {})

    def test_schema_mismatch_is_rejected_when_loading(self) -> None:
        path = self.fixture.registry({}, [], schema=2)
        with self.assertRaises(ValueError):
            coverage.load_registry(path)

    def test_cli_exits_non_zero_and_names_the_gap(self) -> None:
        self.fixture.gate("alpha")
        registry = self.fixture.registry({}, [])
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            exit_code = coverage.main(
                ["--root", str(self.fixture.root), "--registry", str(registry), "--budget", "0"]
            )
        self.assertEqual(exit_code, 1)
        self.assertIn("alpha: invoked by alpha.yml", output.getvalue())


class RepositoryCase(unittest.TestCase):
    """The real registry must describe the real workflows."""

    def test_repository_satisfies_the_ratchet(self) -> None:
        registry = coverage.load_registry()
        self.assertEqual(
            coverage.violations(ROOT, registry, MAX_UNTESTED),
            [],
        )

    def test_untested_budget_never_grows(self) -> None:
        registry = coverage.load_registry()
        self.assertLessEqual(
            len(registry.untested),
            MAX_UNTESTED,
            "a new gate script shipped without a test: cover it, or argue the case "
            "to raise MAX_UNTESTED in this test -- the list is meant to shrink",
        )

    def test_every_registered_gate_still_exists(self) -> None:
        registry = coverage.load_registry()
        missing = [
            stem
            for stem in registry.scripts
            if not (ROOT / "ci" / f"{stem}.py").is_file()
        ]
        self.assertEqual(missing, [])


if __name__ == "__main__":
    unittest.main()

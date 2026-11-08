"""Every CI test module must be executed by a real entry point.

`ci/test_sign_windows_release.py` shipped in #1855 and reached CI only because
someone remembered to append it to the drift workflow's unittest line;
`ci/local_gate.py` kept its own hand-maintained copy of that list and was never
updated, so the local gate silently skipped the new module. Two lists
describing one suite is the defect -- the next module (or the next lane) drifts
again. `ci/local_suites.py` is now the single source of truth, and this module
proves the two entry points agree with it.

The same class of bug has a second face: a `ci/test_*.py` file that no workflow
and no tool ever invokes is a decorative guard (`ci/test_setup_action.py` sat
unused that way until #1844). Every module must therefore be referenced
somewhere, and `KNOWN_UNWIRED` is the explicit, only-shrinking list of the
pre-existing debt.
"""

from __future__ import annotations

import unittest
from pathlib import Path

import yaml

from ci.local_suites import (
    DRIFT_UNITTEST_MODULES,
    KNOWN_UNWIRED,
    LOCAL_ONLY_MODULES,
    LOCAL_UNITTEST_MODULES,
    MAX_UNWIRED,
)

ROOT = Path(__file__).resolve().parents[1]
DRIFT_WORKFLOW = ROOT / ".github" / "workflows" / "ci-workflow-drift.yml"
# The drift step that owns the hosted-CI independence suite. Anchored on the
# step name so a reordering of the workflow cannot silently retarget this test.
DRIFT_STEP_NAME = "Test hosted CI independence from optional local proof"


def drift_workflow() -> dict:
    return yaml.safe_load(DRIFT_WORKFLOW.read_text())


def drift_step_command() -> str:
    matches = [
        step["run"]
        for job in drift_workflow()["jobs"].values()
        for step in job.get("steps", [])
        if step.get("name") == DRIFT_STEP_NAME
    ]
    assert len(matches) == 1, f"expected exactly one step named {DRIFT_STEP_NAME!r}"
    return matches[0]


def unittest_modules(command: str) -> list[str]:
    return [token for token in command.split() if token.startswith("ci.test_")]


def test_module_files() -> set[str]:
    return {path.stem for path in (ROOT / "ci").glob("test_*.py")}


def executable_text() -> str:
    """Every source that can invoke a CI test: workflows and non-test tools."""
    paths = sorted((ROOT / ".github" / "workflows").glob("*.yml"))
    paths += sorted(
        path for path in (ROOT / "ci").glob("*.py") if not path.stem.startswith("test_")
    )
    return "\n".join(path.read_text() for path in paths)


def referenced_anywhere(names: set[str], text: str) -> set[str]:
    return {name for name in names if f"ci.{name}" in text or f"ci/{name}.py" in text}


class DriftRegistryTests(unittest.TestCase):
    def test_drift_workflow_step_matches_registry(self) -> None:
        self.assertEqual(
            unittest_modules(drift_step_command()),
            list(DRIFT_UNITTEST_MODULES),
        )

    def test_local_gate_runs_the_registry_plus_local_only(self) -> None:
        self.assertEqual(
            set(LOCAL_UNITTEST_MODULES),
            set(DRIFT_UNITTEST_MODULES) | set(LOCAL_ONLY_MODULES),
        )
        self.assertLessEqual(set(DRIFT_UNITTEST_MODULES), set(LOCAL_UNITTEST_MODULES))

    def test_local_gate_consumes_the_registry(self) -> None:
        source = (ROOT / "ci" / "local_gate.py").read_text()
        self.assertIn("*LOCAL_UNITTEST_MODULES", source)
        for module in ("ci.test_shared_replay", "ci.test_sign_windows_release"):
            self.assertNotIn(f'"{module}"', source)

    def test_registry_entries_exist_on_disk(self) -> None:
        on_disk = test_module_files()
        for module in LOCAL_UNITTEST_MODULES:
            self.assertIn(module.removeprefix("ci."), on_disk)


class CoverageTests(unittest.TestCase):
    def test_every_ci_test_module_is_executed_somewhere(self) -> None:
        wired = referenced_anywhere(test_module_files(), executable_text())
        self.assertEqual(
            test_module_files() - wired - KNOWN_UNWIRED,
            set(),
            "a ci/test_*.py file that nothing runs is a decorative guard: "
            "register it in the drift workflow and ci/local_suites.py, or add "
            "it to KNOWN_UNWIRED with the reason",
        )

    def test_unwired_budget_never_grows(self) -> None:
        """`KNOWN_UNWIRED` claims to only shrink; without a count it can grow.

        The two tests above both stay green when a brand new module is added to
        the allowlist: the name exists and it is (of course) still unwired. The
        budget is what makes the docstring true -- shrink it with the wiring.
        """
        self.assertLessEqual(
            len(KNOWN_UNWIRED),
            MAX_UNWIRED,
            "a new decorative guard was allowlisted instead of wired: register "
            "it in the drift workflow and ci/local_suites.py, or argue the case "
            "to raise MAX_UNWIRED in this test -- the list is meant to shrink",
        )

    def test_known_unwired_allowlist_has_no_stale_entries(self) -> None:
        wired = referenced_anywhere(test_module_files(), executable_text())
        self.assertEqual(
            KNOWN_UNWIRED - test_module_files(),
            set(),
            "allowlist names a module that no longer exists",
        )
        self.assertEqual(
            KNOWN_UNWIRED & wired,
            set(),
            "drop the now-wired module from KNOWN_UNWIRED",
        )


if __name__ == "__main__":
    unittest.main()

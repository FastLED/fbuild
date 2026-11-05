"""Require a test for every ``ci`` module a workflow invokes as a gate.

The CI self-tests have a coverage guard for test *modules* (``ci/local_suites.py``
+ ``test_local_suites``: a ``ci/test_*.py`` wired into nothing fails).  Nothing
guarded the other direction: a **gate script** that a workflow runs with
``--fail`` (``ci/check_dylint_allowlists.py``, ``ci/check_flash_offsets.py``,
...) could exist with no test at all, so the policy it enforces -- an allowlist,
an offset table, a baseline -- could be loosened or silently stop covering its
input and the only feedback would be a green job.

That is exactly how ``ci/check_python_complexity.py`` sat unguarded until
fbuild#1639 and ``ci/find_direct_subprocess.py`` until #1913: each had two live
contract bugs.  The class is unbounded, so this module replaces
remember-to-add-a-test with a ratchet:

* every module invoked from ``.github/workflows/*.yml`` must appear in
  ``ci/gate_script_tests.toml`` with the test that covers it;
* a module with *no* test must be listed in that registry's ``untested`` array,
  which is a **budget**: the caller may only lower it.  Adding a new unguarded
  gate therefore fails instead of quietly raising the count;
* an entry must be truthful in both directions -- a declared test path must
  exist and mention its subject, and an ``untested`` entry must still have no
  test (once one lands the entry must be removed, so the list only shrinks).

``ci/test_gate_script_coverage.py`` drives these rules against synthetic trees
and asserts the real repository currently satisfies them.
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

REGISTRY_PATH = Path(__file__).resolve().with_name("gate_script_tests.toml")
SCHEMA = 1

# Both invocation spellings used by the workflows: `python ci/foo.py` and
# `python -m ci.foo`.  `-m unittest ci.test_foo` never matches (no `ci.` after
# `-m`), and stems beginning with `test_` are skipped anyway: they are the
# tests, not the gates.
INVOCATION = re.compile(r"(?:ci/([a-z0-9_]+)\.py|-m\s+ci\.([a-z0-9_]+))")


@dataclass(frozen=True)
class Registry:
    """Declared coverage: module stem -> covering test paths, plus the gaps."""

    scripts: dict[str, tuple[str, ...]]
    untested: tuple[str, ...]
    schema: int = SCHEMA


def invoked_modules(root: Path) -> dict[str, tuple[str, ...]]:
    """Gate module stem -> the workflow files that invoke it, both sorted."""

    found: dict[str, set[str]] = {}
    for workflow in sorted((root / ".github" / "workflows").glob("*.yml")):
        for match in INVOCATION.finditer(workflow.read_text(encoding="utf-8")):
            stem = match.group(1) or match.group(2)
            if stem.startswith("test_"):
                continue
            found.setdefault(stem, set()).add(workflow.name)
    return {stem: tuple(sorted(names)) for stem, names in sorted(found.items())}


def load_registry(path: Path = REGISTRY_PATH) -> Registry:
    """Read the registry; raises ``ValueError`` when it is malformed."""

    data = tomllib.loads(path.read_text(encoding="utf-8"))
    scripts = {
        stem: tuple(entry.get("tests", ()))
        for stem, entry in data.get("scripts", {}).items()
    }
    registry = Registry(
        scripts=scripts,
        untested=tuple(data.get("untested", ())),
        schema=int(data.get("schema", 0)),
    )
    if registry.schema != SCHEMA:
        raise ValueError(f"registry schema {registry.schema}, expected {SCHEMA}")
    return registry


def _shape_violations(registry: Registry) -> list[str]:
    """Registry-internal consistency: no duplicate or contradictory entries."""

    problems = []
    for stem in registry.untested:
        tests = registry.scripts.get(stem)
        if tests is None:
            problems.append(f"{stem}: listed as untested but not declared at all")
        elif tests:
            problems.append(f"{stem}: listed as untested but declares tests {list(tests)}")
    return problems


def _inventory_violations(
    invoked: dict[str, tuple[str, ...]], registry: Registry
) -> list[str]:
    """Both directions: no undeclared gate, no registry entry nobody invokes."""

    problems = []
    for stem, workflows in invoked.items():
        if stem not in registry.scripts:
            problems.append(
                f"{stem}: invoked by {', '.join(workflows)} but absent from the registry"
            )
    for stem in registry.scripts:
        if stem not in invoked:
            problems.append(f"{stem}: declared but no workflow invokes it")
    return problems


def _test_reference_violations(root: Path, registry: Registry) -> list[str]:
    """A declared covering test must exist and must mention its subject."""

    problems = []
    for stem, tests in registry.scripts.items():
        for relative in tests:
            path = root / relative
            if not path.is_file():
                problems.append(f"{stem}: declared test {relative} does not exist")
            elif stem not in path.read_text(encoding="utf-8"):
                problems.append(f"{stem}: declared test {relative} never mentions it")
    return problems


def _budget_violations(registry: Registry, budget: int) -> list[str]:
    """The untested list is a budget, not a shipping lane: it may only shrink."""

    if len(registry.untested) > budget:
        return [
            f"{len(registry.untested)} gate modules have no test, budget is {budget}: "
            f"{', '.join(registry.untested)}"
        ]
    return []


def violations(root: Path, registry: Registry, budget: int) -> list[str]:
    """Every way the registry fails to describe this repository, in order."""

    invoked = invoked_modules(root)
    return (
        _shape_violations(registry)
        + _inventory_violations(invoked, registry)
        + _test_reference_violations(root, registry)
        + _budget_violations(registry, budget)
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="repository root (default: this checkout)",
    )
    parser.add_argument(
        "--registry",
        type=Path,
        default=REGISTRY_PATH,
        help="registry TOML (default: this checkout's ci/gate_script_tests.toml)",
    )
    parser.add_argument(
        "--budget",
        type=int,
        required=True,
        help="maximum number of gate modules allowed to have no test",
    )
    args = parser.parse_args(argv)
    registry = load_registry(args.registry)
    problems = violations(args.root.resolve(), registry, args.budget)
    for problem in problems:
        print(problem)
    if problems:
        print(f"\n{len(problems)} gate-coverage problem(s)", file=sys.stderr)
        return 1
    print("Every workflow-invoked gate script is accounted for")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

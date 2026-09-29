"""Every setup-soldr call that can save a cache must own its cache key.

setup-soldr derives its build/target cache keys from toolchain + lockfile +
`cache-key-suffix`. Two jobs on the same runner OS with the same (or no)
suffix therefore share one immutable GitHub cache entry, and whichever job
finishes first on main owns it. On fbuild the embedded-CPython facade job
(166MB, fbuild-python only) beat the workspace Check job's 479MB save, so
every PR restored the wrong cache at a 28-32% hit rate.
"""
from __future__ import annotations

import unittest
from collections import defaultdict
from pathlib import Path

import yaml

WORKFLOWS = Path(__file__).resolve().parent.parent / ".github" / "workflows"

# Suffixes deliberately shared because every owner compiles the identical
# thing, so whichever saves first saves the same content.
SHARED_BY_DESIGN = {
    # `soldr cargo build -p fbuild-cli -p fbuild-daemon` (debug): the shared
    # fbuild_bin job and template_build.yml's standalone fallback.
    "fbuild-rust-debug",
}


def matrix_cells(job):
    """Every concrete matrix combination of `job` (one empty cell if none)."""
    matrix = (job.get("strategy") or {}).get("matrix")
    if not isinstance(matrix, dict):
        return [{}]
    axes = {k: v for k, v in matrix.items() if k not in ("include", "exclude") and isinstance(v, list)}
    cells = [{}]
    for key, values in axes.items():
        cells = [{**cell, key: value} for cell in cells for value in values]
    includes = matrix.get("include") or []
    if includes and not axes:
        cells = [dict(entry) for entry in includes]
    elif includes:
        cells += [dict(entry) for entry in includes]
    return cells


def substitute(value, cell):
    text = str(value)
    for key, cell_value in cell.items():
        for spelling in (f"${{{{ matrix.{key} }}}}", f"${{{{matrix.{key}}}}}"):
            text = text.replace(spelling, str(cell_value))
    return text


def saves(with_):
    # yaml.safe_load turns `save-cache: false` into the boolean False.
    value = with_.get("save-cache", "auto")
    return value is not False and str(value).strip().lower() != "false"


def setup_soldr_calls():
    """(workflow, job, runner, suffix) per saving step and matrix cell."""
    for path in sorted(WORKFLOWS.glob("*.yml")):
        doc = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
        for job_id, job in (doc.get("jobs") or {}).items():
            for step in job.get("steps") or []:
                if "zackees/setup-soldr@" not in str(step.get("uses", "")):
                    continue
                with_ = step.get("with") or {}
                if not saves(with_):
                    continue
                suffix = with_.get("cache-key-suffix")
                for cell in matrix_cells(job):
                    yield (
                        path.name,
                        job_id,
                        substitute(job.get("runs-on"), cell),
                        substitute(suffix, cell) if suffix else None,
                    )


class SetupSoldrCacheKeyTests(unittest.TestCase):
    def test_saving_calls_have_a_suffix(self):
        missing = [f"{wf}:{job}" for wf, job, _, suffix in setup_soldr_calls() if not suffix]
        self.assertEqual([], missing, "setup-soldr steps that save caches need a cache-key-suffix")

    def test_suffixes_are_unique_per_runner(self):
        owners = defaultdict(list)
        for wf, job, runner, suffix in setup_soldr_calls():
            if suffix and suffix not in SHARED_BY_DESIGN:
                owners[(runner, suffix)].append(f"{wf}:{job}")
        shared = {key: jobs for key, jobs in owners.items() if len(jobs) > 1}
        self.assertEqual({}, shared, "jobs sharing a setup-soldr cache key race to own it")

    def test_matrix_cells_expand_include_entries(self):
        job = {"strategy": {"matrix": {"include": [{"bench": "a"}, {"bench": "b"}]}}}
        self.assertEqual(
            ["x-a", "x-b"],
            [substitute("x-${{ matrix.bench }}", cell) for cell in matrix_cells(job)],
        )

    def test_boolean_false_save_cache_is_restore_only(self):
        self.assertFalse(saves({"save-cache": False}))
        self.assertFalse(saves({"save-cache": "false"}))
        self.assertTrue(saves({}))


if __name__ == "__main__":
    unittest.main()

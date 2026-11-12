"""The unittest modules each CI entry point executes.

There are two ways to run the CI self-tests: hosted, from the
``ci-workflow-drift.yml`` step named "Test hosted CI independence from optional
local proof", and natively, from ``python -m ci.local_gate``. Both used to carry
their own hand-maintained copy of the module list, so a module could reach one
and silently miss the other -- ``ci/test_sign_windows_release`` (#1855) ran in
the drift job while the local gate never touched it.

This module is the single source of truth: the drift step runs
``DRIFT_UNITTEST_MODULES`` verbatim, and ``ci/local_gate.py`` runs the same set
plus ``LOCAL_ONLY_MODULES``. ``ci/test_local_suites.py`` fails if either entry
point disagrees, if a listed module does not exist, or if a ``ci/test_*.py``
file is referenced by nothing at all.

Keep the order stable: the drift workflow lists the modules in this order and
the guard compares the sequences.
"""

from __future__ import annotations

DRIFT_UNITTEST_MODULES: tuple[str, ...] = (
    "ci.test_setup_action",
    "ci.test_fractional_workflows",
    "ci.test_local_gate",
    "ci.test_local_dylint_gate",
    "ci.test_local_lint_gate",
    "ci.test_publish_quota",
    "ci.test_gen_mirror_manifest",
    "ci.test_benchmark_publish",
    "ci.test_sign_windows_release",
    "ci.test_check_python_complexity",
    "ci.test_crate_guard_parity",
    "ci.test_find_direct_subprocess",
    "ci.test_audit_ignored_tests",
    "ci.test_local_suites",
    "ci.test_gate_script_coverage",
    "ci.test_check_fbuild_path_baseline",
    "ci.test_check_flash_offsets",
    "ci.test_cancel_superseded",
    "ci.test_check_dylint_allowlists",
    "ci.test_validate_boards",
    "ci.test_build_backend",
    "ci.test_build_qemu_linux_runtime",
    "ci.test_setup_soldr_cache_keys",
    "ci.test_gate_jobs",
    "ci.test_wheel_check",
    "ci.test_check_test_layout",
    "ci.test_lint_entrypoint",
    "ci.test_select_boards",
    "ci.test_run_dylint",
    "ci.test_merge_bench_history",
    "ci.test_publish_bench_site",
)

# Modules the local gate additionally runs. Their subject is the local proof
# lane, and `ci.test_shared_replay` imports the pinned `ci-lint` package that
# only `ci/local_gate.py` installs -- hosted CI deliberately does not.
LOCAL_ONLY_MODULES: tuple[str, ...] = (
    "ci.test_slice_esp_libs",
    "ci.test_slice_toolchain",
    "ci.test_publish_toolchain_slices",
    "ci.test_shared_replay",
    "ci.test_test_cache_status",
)

LOCAL_UNITTEST_MODULES: tuple[str, ...] = DRIFT_UNITTEST_MODULES + LOCAL_ONLY_MODULES

# Pre-existing decorative guards: unit tests whose tool exists and passes, but
# which no workflow or script invokes. They predate this guard and are listed
# here so it fails on NEW ones; wiring them into a lane is separate work.
# The guard fails if an entry disappears, becomes wired, or if the set outgrows
# MAX_UNWIRED, so the list can only shrink.
# The measured debt on 2026-11-06 was seven modules. `test_setup_soldr_cache_keys`
# (the #1843 cache-key collision class), `test_gate_jobs` (the `CI summary`
# aggregator -- the required check that gates every merge here), `test_wheel_check`
# (the release gate that keeps debug info out of the PyPI wheels, #1756's
# stranded-release class), `test_check_test_layout` (the Crate Gate's #1577
# no-stray-test-binary rule), `test_lint_entrypoint` (the `uv run --script lint`
# entry point behind the RUST-033 clippy+dylint rule of #1793) and
# `test_select_boards` (the fail-open diff->board dispatcher: an unknown diff must
# select every board) and `test_run_dylint` (the two-step `soldr dylint prepare`
# -> lint gate that `uv run --script lint` ends with: a failed prepare must stop
# before the lint and report its own exit code) are now wired into the drift
# registry, so the long-standing debt is zero and the list is empty. Adding a name
# here means the class regrew -- wire the module, do not raise the budget.
MAX_UNWIRED = 0

KNOWN_UNWIRED: frozenset[str] = frozenset()

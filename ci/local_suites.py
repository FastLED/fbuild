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
)

# Modules the local gate additionally runs. Their subject is the local proof
# lane, and `ci.test_shared_replay` imports the pinned `ci-lint` package that
# only `ci/local_gate.py` installs -- hosted CI deliberately does not.
LOCAL_ONLY_MODULES: tuple[str, ...] = (
    "ci.test_slice_esp_libs",
    "ci.test_shared_replay",
    "ci.test_test_cache_status",
)

LOCAL_UNITTEST_MODULES: tuple[str, ...] = DRIFT_UNITTEST_MODULES + LOCAL_ONLY_MODULES

# Pre-existing decorative guards: unit tests whose tool exists and passes, but
# which no workflow or script invokes. They predate this guard and are listed
# here so it fails on NEW ones; wiring them into a lane is separate work.
# The guard fails if an entry disappears or becomes wired, so the list can only
# shrink.
KNOWN_UNWIRED: frozenset[str] = frozenset(
    {
        "test_check_test_layout",
        "test_gate_jobs",
        "test_lint_entrypoint",
        "test_run_dylint",
        "test_select_boards",
        "test_setup_soldr_cache_keys",
        "test_wheel_check",
    }
)

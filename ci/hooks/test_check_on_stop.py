#!/usr/bin/env python3
"""Unit tests for the Stop hook's per-crate scoping classifier (issue #465)."""

import importlib.util
import unittest
from pathlib import Path

# The hook script's filename has a dash, so plain `import` won't work —
# load it through importlib instead.
_SCRIPT = Path(__file__).parent / "check-on-stop.py"
_spec = importlib.util.spec_from_file_location("check_on_stop", _SCRIPT)
assert _spec is not None and _spec.loader is not None
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
classify_changes = _module.classify_changes


class ClassifyChangesTests(unittest.TestCase):
    def test_root_cargo_toml_triggers_workspace(self):
        crates, needs_workspace, has_rust = classify_changes(["Cargo.toml"])
        self.assertTrue(needs_workspace)
        self.assertFalse(has_rust)
        self.assertEqual(crates, set())

    def test_cargo_lock_triggers_workspace(self):
        _, needs_workspace, _ = classify_changes(["Cargo.lock"])
        self.assertTrue(needs_workspace)

    def test_toolchain_change_triggers_workspace(self):
        _, needs_workspace, _ = classify_changes(["rust-toolchain.toml"])
        self.assertTrue(needs_workspace)

    def test_dot_cargo_config_triggers_workspace(self):
        _, needs_workspace, _ = classify_changes([".cargo/config.toml"])
        self.assertTrue(needs_workspace)

    def test_crate_change_scopes_to_that_crate(self):
        crates, needs_workspace, has_rust = classify_changes(
            ["crates/fbuild-core/src/lib.rs"]
        )
        self.assertFalse(needs_workspace)
        self.assertTrue(has_rust)
        self.assertEqual(crates, {"fbuild-core"})

    def test_multiple_crate_changes_collect_all(self):
        crates, needs_workspace, _ = classify_changes(
            [
                "crates/fbuild-core/src/lib.rs",
                "crates/fbuild-build/src/symbol_analyzer.rs",
                "crates/fbuild-core/src/symbol_analysis/cref.rs",
            ]
        )
        self.assertFalse(needs_workspace)
        self.assertEqual(crates, {"fbuild-core", "fbuild-build"})

    def test_crate_cargo_toml_does_not_force_workspace(self):
        # A per-crate Cargo.toml change should only re-test that crate;
        # only the ROOT Cargo.toml (which defines workspace members) does.
        crates, needs_workspace, has_rust = classify_changes(
            ["crates/fbuild-core/Cargo.toml"]
        )
        self.assertFalse(needs_workspace, "per-crate Cargo.toml must NOT trigger workspace")
        self.assertEqual(crates, {"fbuild-core"})
        self.assertFalse(has_rust)

    def test_markdown_only_change_skips(self):
        # No Rust files, no workspace triggers — main() should skip,
        # but the classifier still reports the facts.
        crates, needs_workspace, has_rust = classify_changes(
            ["docs/CLAUDE.md", "README.md"]
        )
        self.assertFalse(needs_workspace)
        self.assertFalse(has_rust)
        self.assertEqual(crates, set())

    def test_windows_backslash_paths_are_normalized(self):
        # `git status -z` on Windows can emit backslash separators;
        # the classifier must treat them as `/`.
        crates, _, has_rust = classify_changes(
            ["crates\\fbuild-core\\src\\lib.rs"]
        )
        self.assertTrue(has_rust)
        self.assertEqual(crates, {"fbuild-core"})

    def test_workspace_and_per_crate_change_together_picks_workspace(self):
        # Mixed: Cargo.lock change + crate code change. Workspace wins
        # because cross-crate deps may have shifted under the per-crate
        # code.
        crates, needs_workspace, _ = classify_changes(
            ["Cargo.lock", "crates/fbuild-core/src/lib.rs"]
        )
        self.assertTrue(needs_workspace)
        # crates set is still populated (informational), but the runner
        # will use --workspace anyway.
        self.assertEqual(crates, {"fbuild-core"})

    def test_empty_input_returns_empty(self):
        crates, needs_workspace, has_rust = classify_changes([])
        self.assertFalse(needs_workspace)
        self.assertFalse(has_rust)
        self.assertEqual(crates, set())


if __name__ == "__main__":
    unittest.main()

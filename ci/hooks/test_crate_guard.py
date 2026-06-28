#!/usr/bin/env python3
"""Unit tests for the Cargo.toml-write crate guard hook (#794)."""

from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from crate_guard import (
    APPROVED_CRATE_DIRS,
    _strip_worktree_prefix,
    relative_dir,
)


class StripWorktreePrefixTests(unittest.TestCase):
    """Direct unit coverage of the path-shape transform."""

    def test_strips_worktree_branch_prefix(self) -> None:
        self.assertEqual(
            _strip_worktree_prefix(".claude/worktrees/feat-x/crates/fbuild-build"),
            "crates/fbuild-build",
        )

    def test_no_strip_when_not_under_worktrees(self) -> None:
        self.assertEqual(
            _strip_worktree_prefix("crates/fbuild-build"),
            "crates/fbuild-build",
        )

    def test_returns_dot_when_path_was_only_the_worktree_dir(self) -> None:
        # `.claude/worktrees/<branch>/Cargo.toml` -> after strip, the
        # parent dir is the (empty) worktree root, which `relative_dir`
        # later normalizes to "" (the workspace root manifest slot).
        # `_strip_worktree_prefix` itself returns "." to keep the
        # Path/posix conventions consistent for the caller.
        self.assertEqual(
            _strip_worktree_prefix(".claude/worktrees/feat-x/"),
            ".",
        )

    def test_only_strips_one_segment(self) -> None:
        # Nested `.claude/worktrees/.claude/worktrees/...` should not
        # double-strip — the regex anchors at start and matches once.
        self.assertEqual(
            _strip_worktree_prefix(".claude/worktrees/feat-x/.claude/worktrees/feat-y/crates/foo"),
            ".claude/worktrees/feat-y/crates/foo",
        )

    def test_branch_name_with_hyphens(self) -> None:
        self.assertEqual(
            _strip_worktree_prefix(
                ".claude/worktrees/feat-zccache-embedded-790-phase1/crates/fbuild-build"
            ),
            "crates/fbuild-build",
        )


class RelativeDirIntegrationTests(unittest.TestCase):
    """End-to-end coverage of `relative_dir` with a stubbed repo root."""

    def setUp(self) -> None:
        # Use a real tempdir so `Path.resolve` produces a real abs path
        # the harness can `relative_to` against.
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name).resolve()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _resolve(self, *parts: str) -> str:
        """Materialize a Cargo.toml path under the fake root so
        `Path.resolve()` doesn't tack on a CWD prefix."""
        full = self.root.joinpath(*parts, "Cargo.toml")
        full.parent.mkdir(parents=True, exist_ok=True)
        full.touch()
        return str(full)

    def test_direct_member_path_resolves(self) -> None:
        path = self._resolve("crates", "fbuild-build")
        with patch("crate_guard.repo_root", return_value=self.root):
            self.assertEqual(relative_dir(path), "crates/fbuild-build")

    def test_worktree_member_path_resolves_to_member(self) -> None:
        path = self._resolve(
            ".claude", "worktrees", "feat-zccache-embedded-790-phase1",
            "crates", "fbuild-build",
        )
        with patch("crate_guard.repo_root", return_value=self.root):
            rel = relative_dir(path)
            self.assertEqual(rel, "crates/fbuild-build")
            self.assertIn(rel, APPROVED_CRATE_DIRS)

    def test_worktree_workspace_root_normalizes_to_empty(self) -> None:
        path = self._resolve(".claude", "worktrees", "feat-foo")
        with patch("crate_guard.repo_root", return_value=self.root):
            self.assertEqual(relative_dir(path), "")

    def test_worktree_new_crate_still_rejected(self) -> None:
        path = self._resolve(
            ".claude", "worktrees", "feat-foo",
            "crates", "fbuild-brand-new-crate",
        )
        with patch("crate_guard.repo_root", return_value=self.root):
            rel = relative_dir(path)
            self.assertEqual(rel, "crates/fbuild-brand-new-crate")
            self.assertNotIn(rel, APPROVED_CRATE_DIRS)

    def test_direct_new_crate_still_rejected(self) -> None:
        path = self._resolve("crates", "fbuild-brand-new-crate")
        with patch("crate_guard.repo_root", return_value=self.root):
            rel = relative_dir(path)
            self.assertEqual(rel, "crates/fbuild-brand-new-crate")
            self.assertNotIn(rel, APPROVED_CRATE_DIRS)


if __name__ == "__main__":
    unittest.main()

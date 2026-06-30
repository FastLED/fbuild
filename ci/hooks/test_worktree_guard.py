#!/usr/bin/env python3
"""Unit tests for the worktree-stacking guard (FastLED/fbuild#485)."""

from __future__ import annotations

import io
import json
import os
import sys
import unittest
from unittest.mock import patch

# Import the module under test from the same dir as this test file.
sys.path.insert(0, os.path.dirname(__file__))
from worktree_guard import (  # noqa: E402
    is_inside_worktree,
    normalize_path,
    requests_worktree_isolation,
    session_cwd,
)
import worktree_guard  # noqa: E402


class NormalizePathTests(unittest.TestCase):
    def test_windows_backslashes_become_forward(self) -> None:
        self.assertEqual(
            normalize_path("C:\\Users\\foo\\.claude\\worktrees\\bar"),
            "C:/Users/foo/.claude/worktrees/bar",
        )

    def test_posix_pass_through(self) -> None:
        self.assertEqual(
            normalize_path("/home/foo/.claude/worktrees/bar"),
            "/home/foo/.claude/worktrees/bar",
        )

    def test_empty_string(self) -> None:
        self.assertEqual(normalize_path(""), "")


class IsInsideWorktreeTests(unittest.TestCase):
    def test_direct_worktree_root(self) -> None:
        self.assertTrue(
            is_inside_worktree("/repo/.claude/worktrees/feat-x"),
        )

    def test_subdir_of_worktree(self) -> None:
        self.assertTrue(
            is_inside_worktree("/repo/.claude/worktrees/feat-x/crates/foo"),
        )

    def test_stacked_worktree(self) -> None:
        # A two-deep stack — the case #485 exists to prevent. Single
        # match is enough to deny; we don't care which level matched.
        self.assertTrue(
            is_inside_worktree(
                "/repo/.claude/worktrees/parent/.claude/worktrees/child/crates/foo"
            ),
        )

    def test_windows_path(self) -> None:
        self.assertTrue(
            is_inside_worktree(
                "C:\\Users\\dev\\fbuild\\.claude\\worktrees\\fix-485"
            ),
        )

    def test_main_checkout_is_allowed(self) -> None:
        self.assertFalse(is_inside_worktree("/repo/crates/fbuild-cli"))

    def test_repo_root_itself(self) -> None:
        self.assertFalse(is_inside_worktree("/home/zach/dev/fbuild"))

    def test_empty_cwd_is_allowed(self) -> None:
        self.assertFalse(is_inside_worktree(""))

    def test_path_containing_worktrees_string_but_not_segment(self) -> None:
        # Defensive: a path that literally contains the substring
        # "worktrees" but not as the `.claude/worktrees/<name>/` segment
        # must NOT be matched. The regex requires the `.claude/` prefix.
        self.assertFalse(
            is_inside_worktree("/home/zach/dev/fbuild/docs/worktrees-howto.md"),
        )


class RequestsWorktreeIsolationTests(unittest.TestCase):
    def test_isolation_worktree(self) -> None:
        self.assertTrue(requests_worktree_isolation({"isolation": "worktree"}))

    def test_isolation_worktree_uppercase(self) -> None:
        self.assertTrue(requests_worktree_isolation({"isolation": "Worktree"}))

    def test_isolation_worktree_whitespace(self) -> None:
        self.assertTrue(requests_worktree_isolation({"isolation": "  worktree  "}))

    def test_no_isolation_field(self) -> None:
        self.assertFalse(requests_worktree_isolation({"prompt": "do thing"}))

    def test_isolation_other_value(self) -> None:
        self.assertFalse(requests_worktree_isolation({"isolation": "none"}))

    def test_non_dict_tool_input(self) -> None:
        self.assertFalse(requests_worktree_isolation(None))
        self.assertFalse(requests_worktree_isolation("worktree"))


class SessionCwdTests(unittest.TestCase):
    def test_prefers_cwd_from_payload(self) -> None:
        self.assertEqual(
            session_cwd({"cwd": "/repo/.claude/worktrees/feat"}),
            "/repo/.claude/worktrees/feat",
        )

    def test_falls_back_to_env_when_payload_missing(self) -> None:
        with patch.dict(os.environ, {"CLAUDE_PROJECT_DIR": "/main/repo"}, clear=False):
            self.assertEqual(session_cwd({}), "/main/repo")

    def test_falls_back_to_getcwd_when_nothing_else(self) -> None:
        with patch.dict(os.environ, {}, clear=True):
            self.assertEqual(session_cwd({}), os.getcwd())


class MainIntegrationTests(unittest.TestCase):
    """Drive `main()` end-to-end with a fake stdin payload."""

    def _run_main(self, payload: dict) -> str:
        with (
            patch("sys.stdin", io.StringIO(json.dumps(payload))),
            patch("sys.stdout", new=io.StringIO()) as fake_stdout,
        ):
            try:
                worktree_guard.main()
            except SystemExit as e:
                self.assertEqual(e.code, 0)
            return fake_stdout.getvalue()

    def test_denies_nested_worktree_agent(self) -> None:
        out = self._run_main(
            {
                "tool_name": "Agent",
                "tool_input": {"isolation": "worktree", "prompt": "x"},
                "cwd": "/repo/.claude/worktrees/parent",
            }
        )
        self.assertIn("permissionDecision", out)
        self.assertIn("deny", out)
        self.assertIn("#485", out)

    def test_allows_worktree_isolation_from_main_checkout(self) -> None:
        out = self._run_main(
            {
                "tool_name": "Agent",
                "tool_input": {"isolation": "worktree", "prompt": "x"},
                "cwd": "/repo",
            }
        )
        # No deny verdict written when allowed.
        self.assertEqual(out, "")

    def test_allows_non_worktree_isolation_in_a_worktree(self) -> None:
        out = self._run_main(
            {
                "tool_name": "Agent",
                "tool_input": {"prompt": "x"},
                "cwd": "/repo/.claude/worktrees/feat",
            }
        )
        self.assertEqual(out, "")

    def test_allows_non_agent_tools(self) -> None:
        out = self._run_main(
            {
                "tool_name": "Edit",
                "tool_input": {"isolation": "worktree"},
                "cwd": "/repo/.claude/worktrees/feat",
            }
        )
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Unit tests for the Agent-worktree nesting guard hook."""

import unittest

from worktree_guard import is_inside_worktree, should_deny


class IsInsideWorktreeTests(unittest.TestCase):
    def test_repo_root_is_not_inside_worktree(self):
        self.assertFalse(is_inside_worktree("/c/Users/niteris/dev/fbuild"))
        self.assertFalse(is_inside_worktree("C:\\Users\\niteris\\dev\\fbuild"))

    def test_path_containing_worktree_segment(self):
        self.assertTrue(
            is_inside_worktree("/c/Users/niteris/dev/fbuild/.claude/worktrees/foo")
        )
        self.assertTrue(
            is_inside_worktree("C:\\Users\\niteris\\dev\\fbuild\\.claude\\worktrees\\foo")
        )

    def test_deeply_nested_worktrees(self):
        nested = (
            "C:\\Users\\niteris\\dev\\fbuild\\.claude\\worktrees\\a\\"
            ".claude\\worktrees\\b\\.claude\\worktrees\\c"
        )
        self.assertTrue(is_inside_worktree(nested))

    def test_path_that_only_has_dot_claude_but_not_worktrees(self):
        self.assertFalse(
            is_inside_worktree("/c/Users/niteris/dev/fbuild/.claude/skills")
        )

    def test_trailing_worktrees_dir_without_child_is_not_inside(self):
        # cwd at `.claude/worktrees/` (the parent dir) is not "inside" a
        # specific worktree — a real worktree has a name segment after
        # `worktrees/`. The harness wouldn't put an agent there in practice.
        self.assertFalse(
            is_inside_worktree("/c/Users/niteris/dev/fbuild/.claude/worktrees")
        )


class ShouldDenyTests(unittest.TestCase):
    REPO_ROOT = "C:\\Users\\niteris\\dev\\fbuild"
    INSIDE_WORKTREE = "C:\\Users\\niteris\\dev\\fbuild\\.claude\\worktrees\\agent-aaa"

    def test_denies_agent_worktree_inside_worktree(self):
        self.assertTrue(
            should_deny("Agent", {"isolation": "worktree"}, self.INSIDE_WORKTREE)
        )

    def test_allows_agent_worktree_at_repo_root(self):
        self.assertFalse(
            should_deny("Agent", {"isolation": "worktree"}, self.REPO_ROOT)
        )

    def test_allows_agent_without_worktree_isolation(self):
        self.assertFalse(
            should_deny("Agent", {}, self.INSIDE_WORKTREE),
            "no isolation specified — sub-agent shares cwd, no nesting risk",
        )
        self.assertFalse(
            should_deny("Agent", {"isolation": "none"}, self.INSIDE_WORKTREE),
        )

    def test_ignores_non_agent_tools(self):
        self.assertFalse(
            should_deny("Bash", {"command": "ls"}, self.INSIDE_WORKTREE)
        )
        self.assertFalse(
            should_deny("Edit", {"isolation": "worktree"}, self.INSIDE_WORKTREE),
            "isolation field is meaningless for non-Agent tools",
        )

    def test_tolerates_non_dict_tool_input(self):
        # The JSON event from the harness should always be a dict, but the
        # guard is defensive in case a future event shape ships something
        # else (a list of multi-agent tool uses, for example).
        for bad in (None, "string", 42, ["not", "a", "dict"]):
            with self.subTest(bad=bad):
                self.assertFalse(
                    should_deny("Agent", bad, self.INSIDE_WORKTREE)
                )


if __name__ == "__main__":
    unittest.main()

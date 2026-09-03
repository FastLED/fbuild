#!/usr/bin/env python3
"""Unit tests for ci/check_no_legacy_cross.py.

Run:  uv run --no-project python -m unittest ci.test_no_legacy_cross
"""

from __future__ import annotations

import unittest

from ci.check_no_legacy_cross import PATTERNS, is_comment


def matches(line: str) -> list[str]:
    return [rule for pattern, rule in PATTERNS if pattern.search(line)]


class PatternTests(unittest.TestCase):
    def test_catches_every_retired_backend_invocation(self) -> None:
        for line in [
            "            soldr cargo zigbuild --release --target $TARGET \\",
            "            cargo zigbuild --release --target $TARGET \\",
            "        run: pip install --timeout 60 cargo-zigbuild==0.23.1 ziglang==0.16.0",
            "            cargo xwin build --release --target x86_64-pc-windows-msvc",
            "          tool: cargo-xwin",
            "    zig cc -o out.o in.c",
            "    zig c++ -o out.o in.cc",
        ]:
            with self.subTest(line=line):
                self.assertTrue(matches(line), f"missed: {line}")

    def test_blessed_soldr_commands_are_clean(self) -> None:
        for line in [
            "            soldr prepare --target ${{ inputs.target }}",
            "            soldr build --release --target ${{ inputs.target }} -p fbuild-cli",
            "            PYO3_NO_PYTHON=1 soldr build --release --target $TARGET \\",
            "            soldr cargo build --release -p fbuild-cli",
            "    soldr cc -o out.o in.c",
        ]:
            with self.subTest(line=line):
                self.assertEqual(matches(line), [], f"false positive: {line}")

    def test_unrelated_words_are_not_matched(self) -> None:
        # `zig` as a substring, and xwin/zigbuild inside longer identifiers,
        # must not trip the gate.
        for line in [
            "    let zigzag = compute();",
            "    ZIGBEE_CHANNEL = 15",
            "    print('zigging along')",
        ]:
            with self.subTest(line=line):
                self.assertEqual(matches(line), [], f"false positive: {line}")


class CommentTests(unittest.TestCase):
    def test_history_in_comments_is_allowed(self) -> None:
        # The whole point: the next reader must be able to learn WHY these
        # are gone without the gate firing on the explanation.
        for line in [
            "      # cargo-zigbuild 0.23.4 broke the apple-darwin lanes",
            "        // cargo-xwin needed CRT-casing symlinks",
            "  <!-- zig cc was the old path -->",
        ]:
            with self.subTest(line=line):
                self.assertTrue(is_comment(line), f"not treated as comment: {line}")

    def test_runnable_lines_are_not_comments(self) -> None:
        for line in [
            "        run: cargo zigbuild --release",
            "            soldr build --release",
        ]:
            with self.subTest(line=line):
                self.assertFalse(is_comment(line))


if __name__ == "__main__":
    unittest.main()


class PragmaTests(unittest.TestCase):
    def test_pragma_on_the_line_above_exempts_it(self) -> None:
        from ci.check_no_legacy_cross import is_exempt

        lines = [
            "      # lint-allow: legacy-cross: manylinux glibc floor, soldr cannot hold it",
            "        run: pip install cargo-zigbuild==0.23.1",
        ]
        self.assertTrue(is_exempt(lines, 1))

    def test_unmarked_line_is_not_exempt(self) -> None:
        from ci.check_no_legacy_cross import is_exempt

        lines = ["      # ordinary comment", "        run: pip install cargo-zigbuild"]
        self.assertFalse(is_exempt(lines, 1))

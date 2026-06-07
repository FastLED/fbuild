#!/usr/bin/env python3
"""Unit tests for ci/hooks/_output.py — bounded stderr fed back to Claude
(issue #481).
"""

import importlib.util
import unittest
from pathlib import Path

_SCRIPT = Path(__file__).parent / "_output.py"
_spec = importlib.util.spec_from_file_location("_hook_output", _SCRIPT)
assert _spec is not None and _spec.loader is not None
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
truncate_output = _module.truncate_output
DEFAULT_MAX_LINES = _module.DEFAULT_MAX_LINES


class TruncateOutputTests(unittest.TestCase):
    def test_short_input_passes_through(self):
        self.assertEqual(truncate_output("a\nb\nc", max_lines=10), "a\nb\nc")

    def test_exactly_max_lines_passes_through(self):
        text = "\n".join(str(i) for i in range(5))
        self.assertEqual(truncate_output(text, max_lines=5), text)

    def test_long_input_keeps_tail(self):
        text = "\n".join(str(i) for i in range(500))
        out = truncate_output(text, max_lines=5)
        lines = out.splitlines()
        self.assertEqual(len(lines), 6)
        self.assertEqual(
            lines[0],
            "[... 495 earlier line(s) truncated to fit context ...]",
        )
        self.assertEqual(lines[-1], "499")
        self.assertEqual(lines[-5], "495")

    def test_zero_max_lines_disables_truncation(self):
        text = "\n".join(str(i) for i in range(50))
        self.assertEqual(truncate_output(text, max_lines=0), text)

    def test_negative_max_lines_disables_truncation(self):
        text = "\n".join(str(i) for i in range(50))
        self.assertEqual(truncate_output(text, max_lines=-1), text)

    def test_empty_input(self):
        self.assertEqual(truncate_output("", max_lines=5), "")

    def test_default_max_lines_is_positive(self):
        self.assertGreater(DEFAULT_MAX_LINES, 0)


if __name__ == "__main__":
    unittest.main()

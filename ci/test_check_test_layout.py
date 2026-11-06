"""Tests for ci/check_test_layout.py (FastLED/fbuild#1577)."""
from __future__ import annotations

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_test_layout  # noqa: E402
from check_test_layout import CATEGORIES, MAX_CATEGORIES, violations  # noqa: E402


def make(root: Path, rel: str, body: str = "") -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")


class LayoutTests(unittest.TestCase):
    def test_repository_is_clean(self):
        self.assertEqual([], violations())

    def test_at_most_eight_categories(self):
        self.assertLessEqual(len(CATEGORIES), MAX_CATEGORIES)
        self.assertEqual(8, MAX_CATEGORIES)

    def test_top_level_test_file_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make(root, "crates/fbuild-core/tests/it/main.rs")
            make(root, "crates/fbuild-core/tests/new_thing.rs")
            problems = violations(root)
        self.assertEqual(1, len(problems))
        self.assertIn("new_thing.rs", problems[0])

    def test_unknown_category_dir_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make(root, "crates/fbuild-core/tests/extra/main.rs")
            self.assertIn("unknown test category", violations(root)[0])

    def test_explicit_test_target_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make(root, "crates/fbuild-core/Cargo.toml", '[package]\nname = "x"\n\n[[test]]\nname = "y"\n')
            self.assertIn("[[test]]", violations(root)[0])

    def test_helper_dirs_without_main_are_allowed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make(root, "crates/fbuild-core/tests/it/main.rs")
            make(root, "crates/fbuild-core/tests/fixtures/data.json")
            self.assertEqual([], violations(root))

    def test_test_false_target_with_tests_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make(root, "crates/fbuild-x/Cargo.toml", '[package]\nname = "x"\n\n[lib]\ntest = false\n\n[dependencies]\n')
            make(root, "crates/fbuild-x/src/lib.rs", "mod a;\n")
            make(root, "crates/fbuild-x/src/a.rs", "#[test]\nfn t() {}\n")
            problems = violations(root)
        self.assertEqual(1, len(problems))
        self.assertIn("never runs", problems[0])

    def test_test_false_target_without_tests_is_fine(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make(root, "crates/fbuild-x/Cargo.toml", '[package]\nname = "x"\n\n[[bin]]\nname = "b"\npath = "src/bin/b.rs"\ntest = false\n')
            make(root, "crates/fbuild-x/src/bin/b.rs", "fn main() {}\n")
            self.assertEqual([], violations(root))


def run_main(argv: list[str] | None = None) -> tuple[int, str, str]:
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        rc = check_test_layout.main() if argv is None else check_test_layout.main(argv)
    return rc, out.getvalue(), err.getvalue()


class MainTests(unittest.TestCase):
    """`main()` returns the exit code the Crate Gate obeys.

    The tests above drive `violations()`; nothing asserted that a violation
    actually fails the process, so a `main()` that always returned 0 would
    ship a green unit suite and a gate that permits stray test binaries.
    """

    def test_clean_tree_exits_zero_and_reports_categories(self):
        rc, out, err = run_main()
        self.assertEqual(0, rc)
        self.assertIn(f"ok: {len(CATEGORIES)} integration-test categories", out)
        self.assertEqual("", err)

    def test_one_error_line_per_violation_and_nonzero_exit(self):
        problems = [
            "crates/x/tests/new_thing.rs: top-level test file",
            "crates/y/Cargo.toml: [[test]] targets bypass the category layout",
        ]
        with patch.object(check_test_layout, "violations", return_value=problems):
            rc, out, err = run_main()
        self.assertEqual(1, rc)
        self.assertEqual("", out)
        self.assertEqual(2, err.count("error: "))
        for problem in problems:
            self.assertIn(f"error: {problem}", err)
        self.assertIn("2 test-layout violation(s)", err)


if __name__ == "__main__":
    unittest.main()

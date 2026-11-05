"""Tests for ci/check_fbuild_path_baseline.py -- the shrink-only ratchet.

The gate exists so that `dylints/ban_raw_fbuild_path/src/allowlist.txt` can only
lose entries.  A ratchet that can be switched off by its own arguments is not a
ratchet, so the cases below drive the real `git` subprocess path against a
throwaway repository: additions fail, removals and comment edits pass, and the
two ways a comparison can silently stop happening -- a base ref git cannot
resolve, and an empty base (which `git show` reads as the *index*, i.e. the
working tree compared against itself) -- are hard errors rather than passes.
"""

from __future__ import annotations

import contextlib
import io
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from ci import check_fbuild_path_baseline as baseline

ALLOWLIST = baseline.ALLOWLIST


class Fixture:
    """A throwaway git repository holding one allowlist."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.allowlist = root / ALLOWLIST
        self.allowlist.parent.mkdir(parents=True)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "tests@example.invalid")
        self.git("config", "user.name", "Tests")

    def git(self, *arguments: str) -> str:
        result = subprocess.run(
            ["git", *arguments],
            cwd=self.root,
            capture_output=True,
            text=True,
            check=True,
        )
        return result.stdout.strip()

    def commit(self, message: str) -> str:
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD")

    def write(self, *entries: str) -> None:
        self.allowlist.write_text("\n".join(entries) + "\n", encoding="utf-8")

    def run(self, *arguments: str) -> tuple[int, str, str]:
        """Run main() against this fixture, returning (rc, stdout, stderr)."""
        out, err = io.StringIO(), io.StringIO()
        with (
            mock.patch.object(baseline, "ROOT", self.root),
            contextlib.redirect_stdout(out),
            contextlib.redirect_stderr(err),
        ):
            code = baseline.main(list(arguments))
        return code, out.getvalue(), err.getvalue()


class EntriesTest(unittest.TestCase):
    def test_comments_blanks_and_inline_notes_are_not_entries(self):
        text = (
            "# a header\n"
            "\n"
            "   \n"
            "crates/a/src/lib.rs   # permanent definition site\n"
            "src/b.rs\n"
        )
        self.assertEqual({"crates/a/src/lib.rs", "src/b.rs"}, baseline.entries(text))

    def test_duplicates_collapse_and_order_does_not_matter(self):
        text = "same.rs\nother.rs\nsame.rs\n"
        self.assertEqual({"same.rs", "other.rs"}, baseline.entries(text))


class ResolveTest(unittest.TestCase):
    def test_empty_and_whitespace_refs_never_resolve(self):
        for ref in ("", "   ", "\t"):
            with self.subTest(ref=ref):
                self.assertIsNone(baseline.resolve(ref))

    def test_unknown_ref_does_not_resolve(self):
        self.assertIsNone(baseline.resolve("no-such-ref-anywhere"))

    def test_head_resolves_to_a_commit(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = Fixture(Path(directory))
            fixture.write("src/b.rs")
            head = fixture.commit("base")
            with mock.patch.object(baseline, "ROOT", fixture.root):
                self.assertEqual(head, baseline.resolve("HEAD"))


class RatchetTest(unittest.TestCase):
    def fixture(self, *entries: str) -> tuple[Fixture, str]:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        fixture = Fixture(Path(directory.name))
        fixture.write(*entries)
        base = fixture.commit("base")
        return fixture, base

    def test_unchanged_allowlist_passes(self):
        fixture, base = self.fixture("src/a.rs", "src/b.rs")
        code, out, _ = fixture.run("--base", base)
        self.assertEqual(0, code)
        self.assertIn("2 entries, 0 removed", out)

    def test_added_entry_fails_and_names_the_path(self):
        fixture, base = self.fixture("src/a.rs", "src/b.rs")
        fixture.write("src/a.rs", "src/b.rs", "src/sneaky.rs")  # not committed
        code, _, err = fixture.run("--base", base)
        self.assertEqual(1, code)
        self.assertIn("+ src/sneaky.rs", err)
        self.assertIn("shrink-only", err)

    def test_removed_entry_passes_and_is_reported(self):
        fixture, base = self.fixture("src/a.rs", "src/b.rs")
        fixture.write("src/a.rs")
        code, out, _ = fixture.run("--base", base)
        self.assertEqual(0, code)
        self.assertIn("1 entries, 1 removed", out)

    def test_comment_edit_only_passes(self):
        fixture, base = self.fixture("src/a.rs")
        fixture.allowlist.write_text(
            "# rewritten rationale, no entries changed\nsrc/a.rs\n", encoding="utf-8"
        )
        code, _, _ = fixture.run("--base", base)
        self.assertEqual(0, code)

    def test_unresolvable_base_is_an_error_not_a_pass(self):
        """The original fail-open: a typo'd ref reported 'nothing to ratchet'."""
        fixture, _ = self.fixture("src/a.rs")
        code, out, err = fixture.run("--base", "origin/mian")
        self.assertEqual(1, code)
        self.assertIn("cannot resolve base ref", err)
        self.assertNotIn("nothing to ratchet", out + err)

    def test_empty_base_is_an_error_even_with_a_staged_addition(self):
        """`git show :<path>` is the index, so an empty base compares to itself."""
        fixture, _ = self.fixture("src/a.rs")
        fixture.write("src/a.rs", "src/sneaky.rs")
        fixture.git("add", "-A")  # the index now matches the working tree
        code, _, err = fixture.run("--base", "")
        self.assertEqual(1, code)
        self.assertIn("cannot resolve base ref ''", err)

    def test_base_without_the_allowlist_passes_as_an_introducing_change(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        fixture = Fixture(Path(directory.name))
        (fixture.root / "README.md").write_text("before\n", encoding="utf-8")
        introducing = fixture.commit("before the allowlist existed")
        fixture.write("src/a.rs")
        fixture.commit("introduce the allowlist")
        code, out, _ = fixture.run("--base", introducing)
        self.assertEqual(0, code)
        self.assertIn("nothing to ratchet against yet", out)

    def test_missing_working_tree_allowlist_fails(self):
        fixture, base = self.fixture("src/a.rs")
        fixture.allowlist.unlink()
        code, _, err = fixture.run("--base", base)
        self.assertEqual(1, code)
        self.assertIn("is missing", err)

    def test_working_tree_is_compared_not_just_the_index(self):
        """An unstaged addition must fail: the gate reads the file, not HEAD."""
        fixture, base = self.fixture("src/a.rs")
        fixture.write("src/a.rs", "src/unstaged.rs")
        code, _, err = fixture.run("--base", base)
        self.assertEqual(1, code)
        self.assertIn("+ src/unstaged.rs", err)


class RealRepositoryTest(unittest.TestCase):
    def test_the_repository_satisfies_its_own_ratchet(self):
        root = Path(__file__).resolve().parents[1]
        self.assertTrue((root / ALLOWLIST).is_file())
        code = baseline.main(["--base", "HEAD"])
        self.assertEqual(0, code)


if __name__ == "__main__":
    unittest.main()

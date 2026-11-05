"""Tests for ci/check_dylint_allowlists.py -- the Dylint allowlist gate.

Every lint in ``dylints/*/src`` loads its exemptions with
``include_str!("allowlist.txt")`` and matches them identically: trim the line,
skip blanks and whole-line ``#`` comments, then
``source_path.ends_with(entry)`` against a ``/``-normalized source path. The gate
exists so an entry that cannot possibly match is a hard failure -- otherwise it
is an exemption that silently does not exist, and the developer who added it
believes the lint is silenced while CI keeps rejecting the file.

The cases below are the ways that promise used to be broken, each with a file
that EXISTS on disk so only the parsing rules can catch it:

  * an inline comment (``crates/x.rs  # why``) -- three allowlist headers even
    tell contributors to "include an inline comment" on the entry, and the gate
    stripped it and called the entry current while every lint kept the comment
    as part of the pattern;
  * a backslash entry, in a repository that literally contains such a file --
    the lints compare against a ``\\``-to-``/`` rewritten path, so it never
    matches;
  * an absolute or ``..``-relative entry resolving to a real file outside the
    repository root;
  * an allowlist whose lint never reads it (orphan), so every entry it lists is
    inert.

A root with no allowlists at all is also a failure: a renamed directory must not
print "paths are current".
"""

from __future__ import annotations

import unittest
import unittest.mock
from pathlib import Path
from tempfile import TemporaryDirectory

from ci import check_dylint_allowlists as gate


INCLUDE = 'const ALLOWLIST: &str = include_str!("allowlist.txt");'


def make_lint(root: Path, name: str, allowlist: str, *, loaded: bool = True) -> Path:
    """Create dylints/<name>/src/{lib.rs,allowlist.txt} under this root."""
    src = root / "dylints" / name / "src"
    src.mkdir(parents=True, exist_ok=True)
    lib = "// lint\n" + (INCLUDE + "\n" if loaded else "")
    (src / "lib.rs").write_text(lib, encoding="utf-8")
    text = src / "allowlist.txt"
    text.write_text(allowlist, encoding="utf-8")
    return text


def make_file(root: Path, relative: str) -> Path:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("// source\n", encoding="utf-8")
    return path


class GateCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = TemporaryDirectory()
        self.root = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def problems(self) -> list[str]:
        return gate.check(self.root)


class CleanAllowlistTests(GateCase):
    def test_entry_for_existing_file_passes(self) -> None:
        make_file(self.root, "crates/fbuild-core/src/path.rs")
        make_lint(self.root, "ban_x", "# header\ncrates/fbuild-core/src/path.rs\n")
        self.assertEqual(self.problems(), [])

    def test_comment_only_allowlist_passes(self) -> None:
        make_lint(
            self.root,
            "ban_x",
            "# FastLED/fbuild#844 ships this lint with ZERO entries.\n#\n",
        )
        self.assertEqual(self.problems(), [])

    def test_blank_and_whitespace_lines_are_ignored(self) -> None:
        make_file(self.root, "crates/a/src/lib.rs")
        make_lint(self.root, "ban_x", "\n   \n\t\ncrates/a/src/lib.rs\n")
        self.assertEqual(self.problems(), [])

    def test_trailing_whitespace_is_trimmed_like_the_lint_does(self) -> None:
        make_file(self.root, "crates/a/src/lib.rs")
        make_lint(self.root, "ban_x", "crates/a/src/lib.rs   \t\n")
        self.assertEqual(self.problems(), [])

    def test_reason_on_its_own_comment_line_is_live(self) -> None:
        make_file(self.root, "crates/a/src/lib.rs")
        make_lint(
            self.root, "ban_x", "# why this file is exempt\ncrates/a/src/lib.rs\n"
        )
        self.assertEqual(self.problems(), [])


class MissingFileTests(GateCase):
    def test_missing_path_is_reported(self) -> None:
        make_lint(self.root, "ban_x", "crates/gone/src/lib.rs\n")
        problems = self.problems()
        self.assertEqual(len(problems), 1)
        self.assertIn(gate.MISSING, problems[0])
        self.assertIn("crates/gone/src/lib.rs", problems[0])

    def test_report_names_the_allowlist_line(self) -> None:
        make_lint(self.root, "ban_x", "# header\n\ncrates/gone/src/lib.rs\n")
        self.assertTrue(
            self.problems()[0].startswith("dylints/ban_x/src/allowlist.txt:3 ->")
        )

    def test_directory_is_not_a_file_entry(self) -> None:
        (self.root / "crates" / "a").mkdir(parents=True)
        make_lint(self.root, "ban_x", "crates/a\n")
        self.assertIn(gate.MISSING, self.problems()[0])


class DeadEntryTests(GateCase):
    def test_inline_comment_entry_is_dead_even_though_the_file_exists(self) -> None:
        make_file(self.root, "crates/a/src/lib.rs")
        make_lint(self.root, "ban_x", "crates/a/src/lib.rs  # legit, see #826\n")
        problems = self.problems()
        self.assertEqual(len(problems), 1)
        self.assertIn(gate.DEAD, problems[0])
        self.assertIn("inline comment", problems[0])

    def test_inline_comment_after_aligned_padding_is_dead(self) -> None:
        make_file(self.root, "crates/a/src/lib.rs")
        make_lint(self.root, "ban_x", "crates/a/src/lib.rs\t# tab-separated why\n")
        self.assertIn(gate.DEAD, self.problems()[0])

    def test_backslash_entry_is_dead_even_when_such_a_file_exists(self) -> None:
        make_file(self.root, "crates\\a\\src\\lib.rs")
        make_lint(self.root, "ban_x", "crates\\a\\src\\lib.rs\n")
        problems = self.problems()
        self.assertEqual(len(problems), 1)
        self.assertIn(gate.DEAD, problems[0])
        self.assertIn("backslash", problems[0])

    def test_absolute_entry_is_dead(self) -> None:
        outside = make_file(self.root, "crates/a/src/lib.rs")
        make_lint(self.root, "ban_x", f"{outside}\n")
        problems = self.problems()
        self.assertEqual(len(problems), 1)
        self.assertIn(gate.DEAD, problems[0])
        self.assertIn("absolute", problems[0])

    def test_parent_traversal_entry_is_dead(self) -> None:
        make_file(self.root, "crates/a/src/lib.rs")
        make_lint(self.root, "ban_x", "dylints/../crates/a/src/lib.rs\n")
        problems = self.problems()
        self.assertEqual(len(problems), 1)
        self.assertIn(gate.DEAD, problems[0])
        self.assertIn("escapes", problems[0])

    def test_entry_error_is_none_for_a_plausible_path(self) -> None:
        self.assertIsNone(gate.entry_error("crates/a/src/lib.rs"))


class OrphanAllowlistTests(GateCase):
    def test_entries_in_an_unread_allowlist_are_reported(self) -> None:
        make_file(self.root, "crates/a/src/lib.rs")
        make_lint(self.root, "ban_x", "crates/a/src/lib.rs\n", loaded=False)
        problems = self.problems()
        self.assertEqual(len(problems), 1)
        self.assertIn(gate.ORPHAN, problems[0])

    def test_comment_only_unread_allowlist_is_documentation(self) -> None:
        make_lint(self.root, "ban_x", "# zero entries by design\n", loaded=False)
        self.assertEqual(self.problems(), [])

    def test_allowlist_without_lib_rs_is_orphan(self) -> None:
        make_file(self.root, "crates/a/src/lib.rs")
        src = self.root / "dylints" / "ban_x" / "src"
        src.mkdir(parents=True)
        (src / "allowlist.txt").write_text("crates/a/src/lib.rs\n", encoding="utf-8")
        self.assertIn(gate.ORPHAN, self.problems()[0])

    def test_include_str_in_a_comment_still_counts_as_loaded(self) -> None:
        make_file(self.root, "crates/a/src/lib.rs")
        src = self.root / "dylints" / "ban_x" / "src"
        src.mkdir(parents=True)
        (src / "lib.rs").write_text(f"// {INCLUDE}\n", encoding="utf-8")
        (src / "allowlist.txt").write_text("crates/a/src/lib.rs\n", encoding="utf-8")
        self.assertEqual(self.problems(), [])


class VacuityTests(GateCase):
    def test_no_allowlists_is_a_failure_not_a_pass(self) -> None:
        problems = self.problems()
        self.assertEqual(len(problems), 1)
        self.assertIn("no Dylint allowlists found", problems[0])

    def test_dylints_dir_absent_is_a_failure(self) -> None:
        (self.root / "crates").mkdir()
        self.assertIn("no Dylint allowlists found", self.problems()[0])


class ExitCodeTests(GateCase):
    def _run_main(self) -> int:
        with unittest.mock.patch.object(gate, "ROOT", self.root):
            return gate.main()

    def test_clean_root_exits_zero(self) -> None:
        make_file(self.root, "crates/a/src/lib.rs")
        make_lint(self.root, "ban_x", "crates/a/src/lib.rs\n")
        self.assertEqual(self._run_main(), 0)

    def test_problem_root_exits_one(self) -> None:
        make_lint(self.root, "ban_x", "crates/gone/src/lib.rs\n")
        self.assertEqual(self._run_main(), 1)


class RealRepositoryTests(unittest.TestCase):
    def test_every_allowlist_in_this_repository_is_current(self) -> None:
        self.assertEqual(gate.check(gate.ROOT), [])

    def test_the_glob_still_finds_the_whole_inventory(self) -> None:
        allowlists = gate.allowlist_files(gate.ROOT)
        self.assertGreaterEqual(len(allowlists), 24)
        for allowlist in allowlists:
            has_entries = any(
                gate.entry_of(raw) for raw in allowlist.read_text().splitlines()
            )
            self.assertTrue(
                not has_entries or gate.lint_reads(allowlist),
                f"{allowlist} lists exemptions its lint never loads",
            )

    def test_repository_entries_never_carry_an_inline_comment(self) -> None:
        for allowlist in gate.allowlist_files(gate.ROOT):
            for raw in allowlist.read_text().splitlines():
                entry = gate.entry_of(raw)
                if entry:
                    self.assertIsNone(gate.entry_error(entry), f"{allowlist}: {raw!r}")

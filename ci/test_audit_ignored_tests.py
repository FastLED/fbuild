"""Tests for ci/audit_ignored_tests.py (FastLED/fbuild#839).

The weekly `Audit Ignored Tests` workflow publishes this inventory as the body
of tracking issue #880, so the report is the only thing Zach reads.  It had no
test: nothing asserted that a real `#[ignore]` is counted, that a fixture *inside*
a string literal is not, or that a `'"'` char literal does not blank the rest of
a line.  These tests drive the production scanner over synthetic Rust files, the
lexer in isolation, and the live workspace.
"""

from __future__ import annotations

import csv
import io
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from audit_ignored_tests import (  # noqa: E402
    code_only,
    collect_inventory,
    emit_csv,
    emit_markdown,
    iter_rust_files,
)

REPO_ROOT = Path(__file__).resolve().parent.parent


def audit_text(text: str, name: str = "sample.rs") -> list[dict]:
    """Run the production scanner over one synthetic source file."""
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return list(collect_inventory(root))


class IgnoreDetectionTest(unittest.TestCase):
    def test_bare_ignore_is_a_violation_with_no_reason(self) -> None:
        findings = audit_text("#[test]\n#[ignore]\nfn slow_case() {}\n")
        self.assertEqual(len(findings), 1)
        self.assertEqual(findings[0]["reason"], "")
        self.assertEqual(findings[0]["test_name"], "slow_case")
        self.assertEqual(findings[0]["line"], 2)

    def test_reason_string_is_captured(self) -> None:
        findings = audit_text('#[ignore = "needs a board"]\n#[test]\nfn hw() {}\n')
        self.assertEqual([(f["reason"], f["test_name"]) for f in findings], [("needs a board", "hw")])

    def test_raw_string_reason_is_captured(self) -> None:
        findings = audit_text('#[ignore = r#"needs a "board" and a bus"#]\nfn hw() {}\n')
        self.assertEqual([f["reason"] for f in findings], ['needs a "board" and a bus'])

    def test_trailing_line_comment_after_attribute_is_allowed(self) -> None:
        findings = audit_text('#[ignore = "why"] // tracked in #839\nfn hw() {}\n')
        self.assertEqual([f["reason"] for f in findings], ["why"])

    def test_attribute_must_end_its_line(self) -> None:
        # `#[test] #[ignore]` on one line is not recorded: the reason and the
        # fn would both have to be guessed.  Documented contract, not a claim
        # that such code exists.
        self.assertEqual(audit_text("#[test] #[ignore]\nfn hw() {}\n"), [])

    def test_non_literal_reason_is_not_reported_as_bare(self) -> None:
        # A policy violation must never be invented from an attribute this
        # parser does not understand.
        self.assertEqual(audit_text('#[ignore = concat!("a", "b")]\nfn hw() {}\n'), [])

    def test_unparseable_test_name_is_empty_not_invented(self) -> None:
        findings = audit_text("#[ignore]\nslow_testcase! {\n}\n")
        self.assertEqual([f["test_name"] for f in findings], [""])

    def test_fn_name_search_skips_attributes_and_doc_comments(self) -> None:
        text = '#[ignore]\n/// docs\n#[cfg(unix)]\n#[test]\npub async fn flagged() {}\n'
        self.assertEqual([f["test_name"] for f in audit_text(text)], ["flagged"])

    def test_findings_are_sorted_by_file_then_line(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "b.rs").write_text("#[ignore]\nfn b() {}\n", encoding="utf-8")
            (root / "a.rs").write_text("#[ignore]\nfn a1() {}\n#[ignore]\nfn a2() {}\n", encoding="utf-8")
            findings = collect_inventory(root)
        self.assertEqual([(f["file"], f["line"]) for f in findings], [("a.rs", 1), ("a.rs", 3), ("b.rs", 1)])


class NotAnAttributeTest(unittest.TestCase):
    """Text that merely spells `#[ignore]` must never enter the inventory."""

    def test_line_comment_mention_is_ignored(self) -> None:
        self.assertEqual(audit_text("// TODO: #[ignore] this\nfn hw() {}\n"), [])

    def test_block_comment_mention_is_ignored(self) -> None:
        text = "/* why #[ignore]\n   #[ignore]\n*/\nfn hw() {}\n"
        self.assertEqual(audit_text(text), [])

    def test_nested_block_comment_mention_is_ignored(self) -> None:
        text = "/* outer /* inner */ #[ignore] */\nfn hw() {}\n"
        self.assertEqual(audit_text(text), [])

    def test_raw_string_fixture_is_not_a_real_ignore(self) -> None:
        # The class of bug a line-start regex cannot see: a test fixture holding
        # Rust source.  Counting it would publish a phantom ignored test.
        text = 'const FIXTURE: &str = r#"\n#[ignore]\nfn fixture_fn() {}\n"#;\n'
        self.assertEqual(audit_text(text), [])

    def test_escaped_quote_does_not_swallow_the_next_line(self) -> None:
        # A mishandled backslash ends the string at the escaped quote and the
        # NEXT literal swallows the rest of the file, hiding real attributes.
        text = 'let s = "a ' + chr(92) + '" b";' + chr(10) + '#[ignore]' + chr(10)
        self.assertEqual(code_only(text).splitlines()[1], "#[ignore]")

    def test_normal_string_fixture_is_not_a_real_ignore(self) -> None:
        self.assertEqual(audit_text('const S: &str = "#[ignore]";\nfn hw() {}\n'), [])

    def test_char_literal_quote_does_not_blank_the_file(self) -> None:
        # `'"'` used to open a phantom string.  The real attribute below it must
        # still be found.
        text = "fn quote() -> char { '\"' }\n#[ignore]\nfn hw() {}\n"
        self.assertEqual([f["test_name"] for f in audit_text(text)], ["hw"])

    def test_escaped_char_literal_does_not_hide_a_real_ignore(self) -> None:
        text = "fn quote() -> char { '\\'' }\n#[ignore]\nfn hw() {}\n"
        self.assertEqual([f["test_name"] for f in audit_text(text)], ["hw"])

    def test_lifetime_is_not_a_char_literal(self) -> None:
        # `&'a str` must not be treated as an unterminated char literal.
        text = "fn borrow<'a>(s: &'a str) -> &'a str { s }\n#[ignore]\nfn hw() {}\n"
        self.assertEqual([f["test_name"] for f in audit_text(text)], ["hw"])


class CodeOnlyTest(unittest.TestCase):
    def test_blanking_preserves_columns_and_newlines(self) -> None:
        text = 'let s = "abc"; // tail\n#[ignore]\n'
        blanked = code_only(text)
        self.assertEqual(blanked.count("\n"), 2)
        self.assertEqual(len(blanked), len(text))
        self.assertEqual(blanked.splitlines()[1], "#[ignore]")

    def test_string_contents_are_blanked_without_touching_neighbours(self) -> None:
        blanked = code_only('f("x");\n')
        self.assertEqual(blanked, "f(   );\n")

    def test_raw_string_spans_lines(self) -> None:
        text = 'let s = r#"\n#[ignore]\n"#;\n#[ignore]\nfn hw() {}\n'
        blanked = code_only(text)
        self.assertNotIn("#[ignore]", blanked.splitlines()[1])
        self.assertEqual(blanked.splitlines()[3], "#[ignore]")


class SkipDirTest(unittest.TestCase):
    def test_build_and_vcs_dirs_are_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for sub in ("target", ".git", "src"):
                (root / sub).mkdir()
                (root / sub / "x.rs").write_text("#[ignore]\nfn x() {}\n", encoding="utf-8")
            relative = sorted(path.relative_to(root).as_posix() for path in iter_rust_files(root))
            self.assertEqual(relative, ["src/x.rs"])

    def test_scan_root_under_a_skipped_dir_name_is_still_scanned(self) -> None:
        # Regression: the skip set was matched against ABSOLUTE path components,
        # so a checkout living under `.../.claude/worktrees/...` or inside a
        # directory named `target` reported an empty inventory instead of erroring.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / ".claude" / "worktrees" / "proj"
            (root / "src").mkdir(parents=True)
            (root / "src" / "lib.rs").write_text("#[ignore]\nfn x() {}\n", encoding="utf-8")
            findings = collect_inventory(root)
            self.assertEqual([f["file"] for f in findings], ["src/lib.rs"])


class ReportShapeTest(unittest.TestCase):
    FINDINGS = [
        {"file": "a.rs", "test_name": "a", "reason": "why", "line": 3},
        {"file": "a.rs", "test_name": "", "reason": "", "line": 9},
        {"file": "b.rs", "test_name": "b", "reason": "a | b", "line": 1},
    ]

    def test_csv_round_trips_quoting(self) -> None:
        buf = io.StringIO()
        emit_csv(self.FINDINGS, buf)
        rows = list(csv.DictReader(io.StringIO(buf.getvalue())))
        self.assertEqual([r["file"] for r in rows], ["a.rs", "a.rs", "b.rs"])
        self.assertEqual(rows[2]["reason"], "a | b")
        self.assertEqual(buf.getvalue().splitlines()[0], "file,test_name,reason,line")

    def test_markdown_summarises_policy_violations(self) -> None:
        buf = io.StringIO()
        emit_markdown(self.FINDINGS, buf)
        report = buf.getvalue()
        self.assertIn("**Total `#[ignore]` findings:** 3", report)
        self.assertIn("**Missing reason string (policy violation per #839):** 1", report)
        self.assertIn("_(no reason — policy violation)_", report)
        self.assertIn("_(unparseable)_", report)
        self.assertIn("`a \\| b`".replace("`", ""), report)

    def test_empty_inventory_still_renders_a_table(self) -> None:
        buf = io.StringIO()
        emit_markdown([], buf)
        self.assertIn("**Total `#[ignore]` findings:** 0", buf.getvalue())


class LiveWorkspaceTest(unittest.TestCase):
    def test_every_reported_line_really_carries_the_attribute(self) -> None:
        findings = collect_inventory(REPO_ROOT)
        self.assertGreater(len(findings), 50, "the real workspace has an ignore inventory")
        for finding in findings:
            path = REPO_ROOT / finding["file"]
            self.assertTrue(path.is_file(), finding["file"])
            line = path.read_text(encoding="utf-8").splitlines()[finding["line"] - 1]
            self.assertIn("#[ignore", line, f"{finding['file']}:{finding['line']}")
            self.assertNotIn("/target/", f"/{finding['file']}")

    def test_cli_emits_markdown_for_a_given_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "s.rs").write_text('#[ignore = "reasons"]\nfn hw() {}\n', encoding="utf-8")
            proc = subprocess.run(
                [
                    sys.executable,
                    str(REPO_ROOT / "ci" / "audit_ignored_tests.py"),
                    "--workspace-root",
                    tmp,
                    "--markdown",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("**Total `#[ignore]` findings:** 1", proc.stdout)
        self.assertIn("`s.rs`", proc.stdout)


if __name__ == "__main__":
    unittest.main()

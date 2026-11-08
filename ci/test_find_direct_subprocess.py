"""Tests for ci/find_direct_subprocess.py (FastLED/fbuild#141).

The gate runs with ``--fail`` in ``check-ubuntu.yml`` and
``lint-subprocess.yml``, so both of its faces are load-bearing and neither was
covered: a skip that is too eager turns an innocent comment or test fixture
into a red build, and an allowlist that accepts a bare marker silently
pre-authorises every future direct spawn at that site.  These tests drive the
production scanner over synthetic Rust files and the real workspace.
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from find_direct_subprocess import (  # noqa: E402
    Hit,
    code_only,
    exit_code,
    scan_file,
    scan_workspace,
    unmigrated,
)

SPAWN = 'let child = Command::new("git");\n'
MARKED = "// allow-direct-spawn: daemon must outlive the CLI\n"


def scan_text(text: str) -> list[Hit]:
    """Run the production scanner over one synthetic source file."""
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "sample.rs"
        path.write_text(text, encoding="utf-8")
        return scan_file(path)


class DirectSpawnDetectionTest(unittest.TestCase):
    def test_unmarked_spawn_is_a_violation(self) -> None:
        hits = scan_text(SPAWN)
        self.assertEqual(len(hits), 1)
        self.assertFalse(hits[0].allowlisted)
        self.assertEqual(hits[0].line_no, 1)
        self.assertEqual(len(unmigrated(hits)), 1)

    def test_spawn_forms_are_all_matched(self) -> None:
        text = (
            'std::process::Command::new("a");\n'
            'tokio::process::Command::new("b");\n'
            'Command::new("c");\n'
        )
        self.assertEqual(len(scan_text(text)), 3)

    def test_reason_marker_on_same_line_allowlists(self) -> None:
        hits = scan_text('let c = Command::new("git"); // allow-direct-spawn: detached editor\n')
        self.assertEqual([(h.allowlisted, h.reason) for h in hits], [(True, "detached editor")])
        self.assertEqual(unmigrated(hits), [])

    def test_reason_marker_on_previous_line_allowlists(self) -> None:
        hits = scan_text(MARKED + SPAWN)
        self.assertTrue(hits[0].allowlisted)
        self.assertEqual(hits[0].reason, "daemon must outlive the CLI")

    def test_marker_without_a_reason_does_not_allowlist(self) -> None:
        # Regression: this used to come back allowlisted as "<no reason given>",
        # which made a forgotten migration indistinguishable from a hold-out.
        for spelling in ("// allow-direct-spawn\n", "// allow-direct-spawn:\n", "// allow-direct-spawn:   \r\n"):
            with self.subTest(spelling=spelling):
                hits = scan_text(spelling + SPAWN)
                self.assertEqual(len(hits), 1)
                self.assertFalse(hits[0].allowlisted)
                self.assertIsNone(hits[0].reason)
                self.assertEqual(len(unmigrated(hits)), 1)


class DocumentedMentionsAreNotSpawnsTest(unittest.TestCase):
    def test_trailing_line_comment_is_ignored(self) -> None:
        self.assertEqual(scan_text('let x = 1; // e.g. Command::new("git")\n'), [])

    def test_whole_line_doc_comments_are_ignored(self) -> None:
        text = '/// Example: Command::new("git")\n//! Command::new("git")\n// Command::new("git")\n'
        self.assertEqual(scan_text(text), [])

    def test_block_comment_spanning_lines_is_ignored(self) -> None:
        text = '/*\n Command::new("git")\n*/\nlet x = 1;\n'
        self.assertEqual(scan_text(text), [])

    def test_nested_block_comment_is_ignored(self) -> None:
        text = '/* outer /* inner */ Command::new("git") */\nlet x = 1;\n'
        self.assertEqual(scan_text(text), [])

    def test_string_literals_are_ignored(self) -> None:
        text = (
            'let a = "Command::new(x)";\n'
            'let b = "escaped \\" Command::new(x)";\n'
            'let c = r#"Command::new(x)"#;\n'
            'let d = r"Command::new(x)";\n'
        )
        self.assertEqual(scan_text(text), [])

    def test_char_literal_does_not_open_a_phantom_string(self) -> None:
        # `'"'` holds a quote character: treating it as a string opener used to
        # blank the rest of the line, hiding the real spawn that follows it.
        text = "let q = '\"'; " + SPAWN.lstrip()
        hits = scan_text(text)
        self.assertEqual([h.line_no for h in hits], [1])

    def test_lifetime_does_not_swallow_code(self) -> None:
        text = "fn f<'a>(x: &'a str) -> i32 { let c = Command::new(\"git\"); 0 }\n"
        self.assertEqual(len(scan_text(text)), 1)

    def test_real_spawn_beside_a_documented_one_is_the_only_hit(self) -> None:
        text = 'let s = "Command::new(x)"; // Command::new(y)\n' + SPAWN
        hits = scan_text(text)
        self.assertEqual([h.line_no for h in hits], [2])

    def test_code_only_preserves_columns_and_depth(self) -> None:
        line = 'let a = "x"; /* open'
        code, depth = code_only(line, 0)
        self.assertEqual(len(code), len(line))
        self.assertEqual(code.rstrip(), "let a =    ;")
        self.assertEqual(depth, 1)
        closed, depth = code_only('*/ Command::new("z");', depth)
        self.assertEqual(depth, 0)
        self.assertTrue(closed.lstrip().startswith("Command::new("))
        self.assertEqual(len(closed), len('*/ Command::new("z");'))


class ExitCodeTest(unittest.TestCase):
    def test_fail_flag_decides_the_exit_code(self) -> None:
        pending = scan_text(SPAWN)
        allowed = scan_text(MARKED + SPAWN)
        self.assertEqual(exit_code(pending, fail=True), 1)
        self.assertEqual(exit_code(pending, fail=False), 0)
        self.assertEqual(exit_code(allowed, fail=True), 0)


class RepositorySatisfiesTheGateTest(unittest.TestCase):
    def test_no_unannotated_spawn_on_the_workspace(self) -> None:
        hits = scan_workspace()
        self.assertTrue(hits, "the workspace scan found no spawn sites at all")
        self.assertEqual(
            [f"{h.path.name}:{h.line_no}" for h in unmigrated(hits)],
            [],
            "every direct spawn site must carry `allow-direct-spawn: <reason>`",
        )

    def test_every_allowlisted_site_carries_a_reason(self) -> None:
        for hit in scan_workspace():
            with self.subTest(path=str(hit.path), line=hit.line_no):
                self.assertTrue(hit.allowlisted)
                self.assertEqual(hit.reason, (hit.reason or "").strip())
                self.assertTrue(hit.reason)


if __name__ == "__main__":
    unittest.main()

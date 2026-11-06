"""Parity tests for the monocrate guard's hand-maintained allowlists.

The "no new crates" policy is enforced twice, from two different files:

* ``ci/check_workspace_crates.py`` (batch, ``crate-gate.yml``) holds
  ``APPROVED_MEMBERS`` and compares it against the root ``Cargo.toml``
  ``[workspace] members``.
* ``ci/hooks/crate_guard.py`` (real time, PreToolUse) holds
  ``APPROVED_CRATE_DIRS`` and denies an ``Edit``/``Write`` of any
  ``Cargo.toml`` outside it.

Both files tell readers to "keep the two lists in sync", but nothing asserted
it. The real-time list had already drifted: ``dylints/enforce_platform_boundary``
is a ``[workspace] exclude`` entry with a real ``Cargo.toml``, yet it was missing
from ``APPROVED_CRATE_DIRS``, so the hook denied every edit to the permanent
platform-boundary dylint with a "you are creating a new crate" message -- a
blocking false positive, not a policy. That is the same bug class as
FastLED/fbuild#794 ("a worktree of this repo is still this repo with the same
approved-crates contract"): an allowlist that misdescribes the repository.

So this module asserts the repository facts, not the list text:

* the batch allowlist is exactly the workspace ``members`` set;
* every ``[workspace] exclude`` directory that actually contains a
  ``Cargo.toml`` is writable through the hook;
* the hook adds nothing beyond ``members`` + ``exclude`` + the repo root, so a
  stale entry cannot pre-authorize a crate somebody adds later;
* the hook's real decision (allow vs deny JSON) agrees with both directions.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CARGO_TOML = ROOT / "Cargo.toml"
HOOK = ROOT / "ci" / "hooks" / "crate_guard.py"
BATCH_CHECK = ROOT / "ci" / "check_workspace_crates.py"
FROZENSET_RE = r"[^\n]*=\s*frozenset\(\s*\{(.*?)\n\s*\}\s*\)"


def array_values(source: str, key: str) -> list[str]:
    """Extract every quoted string from the ``key = [ ... ]`` array."""
    key_index = source.index(f"{key} = [")
    open_index = source.index("[", key_index)
    close_index = source.index("\n]", open_index)
    body = re.sub(r"#[^\n]*", "", source[open_index:close_index])
    return re.findall(r'"([^"]*)"', body)


def frozenset_values(path: Path, name: str) -> set[str]:
    """Extract a ``frozenset({...})`` literal without importing the module."""
    source = path.read_text(encoding="utf-8")
    match = re.search(name + FROZENSET_RE, source, re.DOTALL)
    if match is None:
        raise AssertionError(f"{path} has no {name} frozenset literal")
    return set(re.findall(r'"([^"]*)"', re.sub(r"#[^\n]*", "", match.group(1))))


def workspace_lists() -> tuple[set[str], set[str]]:
    source = CARGO_TOML.read_text(encoding="utf-8")
    return set(array_values(source, "members")), set(array_values(source, "exclude"))


def approved_members() -> set[str]:
    return frozenset_values(BATCH_CHECK, "APPROVED_MEMBERS")


def hook_dirs() -> set[str]:
    return frozenset_values(HOOK, "APPROVED_CRATE_DIRS")


def cargo_projects(directories: set[str]) -> set[str]:
    return {d for d in directories if (ROOT / d / "Cargo.toml").is_file()}


def run_hook(manifest: Path) -> str:
    """Drive the real hook exactly as the harness does: JSON on stdin."""
    payload = {
        "tool_name": "Write",
        "tool_input": {"file_path": str(manifest)},
    }
    completed = subprocess.run(
        [sys.executable, str(HOOK)],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        cwd=str(ROOT),
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    return completed.stdout.strip()


def decision(stdout: str) -> str:
    if not stdout:
        return "allow"
    return json.loads(stdout)["hookSpecificOutput"]["permissionDecision"]


class AllowlistParityTests(unittest.TestCase):
    def test_batch_allowlist_is_exactly_the_workspace_members(self) -> None:
        members, _ = workspace_lists()
        self.assertEqual(approved_members(), members)

    def test_every_excluded_cargo_project_is_writable_through_the_hook(self) -> None:
        _, exclude = workspace_lists()
        unapproved = cargo_projects(exclude) - hook_dirs()
        self.assertEqual(unapproved, set())

    def test_hook_allowlist_adds_no_unreviewed_directory(self) -> None:
        members, exclude = workspace_lists()
        extra = hook_dirs() - members - exclude - {""}
        self.assertEqual(extra, set())

    def test_hook_allowlist_has_no_rotted_entry(self) -> None:
        rotted = hook_dirs() - {""} - cargo_projects(hook_dirs() | {""})
        self.assertEqual(rotted, set())


class HookDecisionTests(unittest.TestCase):
    def test_workspace_root_manifest_is_allowed(self) -> None:
        self.assertEqual(decision(run_hook(CARGO_TOML)), "allow")

    def test_excluded_dylint_manifest_is_allowed(self) -> None:
        manifest = ROOT / "dylints" / "enforce_platform_boundary" / "Cargo.toml"
        self.assertEqual(decision(run_hook(manifest)), "allow")

    def test_brand_new_crate_manifest_is_denied(self) -> None:
        manifest = ROOT / "crates" / "fbuild-not-approved" / "Cargo.toml"
        self.assertEqual(decision(run_hook(manifest)), "deny")


sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_workspace_crates import (  # noqa: E402
    RATCHET_LINTS,
    inherits_workspace_lints,
    lint_inheritance_errors,
    parse_members,
)

WORKSPACE_LINTS = """
[workspace.lints.clippy]
cognitive_complexity = "warn"
too_many_lines = "warn"
"""

INHERITING = """
[lints]
workspace = true
"""


def synthetic_workspace(
    root: Path, members: dict[str, str], lints: str = WORKSPACE_LINTS
) -> Path:
    """Write a minimal root manifest plus one manifest per named member."""
    body = "".join(f'    "{name}",\n' for name in members)
    manifest = f"[workspace]\nmembers = [\n{body}]\n{lints}"
    (root / "Cargo.toml").write_text(manifest, encoding="utf-8")
    for name, text in members.items():
        directory = root / name
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "Cargo.toml").write_text(text, encoding="utf-8")
    return root


class LintInheritanceTests(unittest.TestCase):
    """RUST-018 (FastLED/fbuild#1639): inheritance is a checked invariant."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="lint-inheritance-")
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)

    def errors(
        self, members: dict[str, str], lints: str = WORKSPACE_LINTS
    ) -> list[str]:
        return lint_inheritance_errors(synthetic_workspace(self.root, members, lints))

    def test_real_workspace_inherits_every_ratchet(self) -> None:
        members = parse_members(CARGO_TOML.read_text(encoding="utf-8"))
        # 20 crates/* plus bench/fastled-examples: the inheritance half of
        # #1639 claimed all 21 members, so a shrinking loop cannot pass.
        self.assertEqual(len(members), 21)
        for member in members:
            self.assertTrue((ROOT / member / "Cargo.toml").is_file(), member)
            self.assertTrue(
                inherits_workspace_lints(
                    (ROOT / member / "Cargo.toml").read_text(encoding="utf-8")
                ),
                f"{member} lost its [lints] workspace = true",
            )
        self.assertEqual(lint_inheritance_errors(ROOT), [])

    def test_member_without_lints_table_is_named(self) -> None:
        errors = self.errors(
            {"crates/a": INHERITING, "crates/b": '[package]\nname = "b"\n'}
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("crates/b", errors[0])
        self.assertNotIn("crates/a", errors[0])

    def test_commented_out_inheritance_is_not_accepted(self) -> None:
        errors = self.errors({"crates/a": "# [lints]\n# workspace = true\n"})
        self.assertEqual(len(errors), 1)
        self.assertIn("crates/a", errors[0])

    def test_workspace_false_is_not_inheritance(self) -> None:
        errors = self.errors({"crates/a": "[lints]\nworkspace = false\n"})
        self.assertEqual(len(errors), 1)
        self.assertIn("crates/a", errors[0])

    def test_dotted_form_and_trailing_comment_are_accepted(self) -> None:
        errors = self.errors(
            {
                "crates/a": "lints.workspace = true\n",
                "crates/b": INHERITING.replace(
                    "workspace = true", "workspace = true  # RUST-018"
                ),
            }
        )
        self.assertEqual(errors, [])

    def test_ratchet_downgrade_to_allow_is_reported(self) -> None:
        downgraded = WORKSPACE_LINTS.replace('cognitive_complexity = "warn"', 'cognitive_complexity = "allow"')
        errors = self.errors({"crates/a": INHERITING}, downgraded)
        self.assertEqual(len(errors), 1)
        self.assertIn("cognitive_complexity", errors[0])
        self.assertIn("allow", errors[0])

    def test_missing_ratchet_lint_is_reported(self) -> None:
        dropped = WORKSPACE_LINTS.replace(
            'cognitive_complexity = "warn"\n', ""
        ).replace('too_many_lines = "warn"', 'too_many_lines = "warn"')
        self.assertNotIn("cognitive_complexity", dropped)
        errors = self.errors({"crates/a": INHERITING}, dropped)
        self.assertEqual(len(errors), 1)
        self.assertIn(RATCHET_LINTS[0], errors[0])


if __name__ == "__main__":
    unittest.main()

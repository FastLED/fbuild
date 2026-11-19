"""Tests for ci/check_python_types.py: every tracked .py passes basedpyright.

The production command is driven against synthetic modules, so a loosened
mode, a dropped rule or an unresolvable dependency fails here instead of the
gate quietly passing.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_python_types import (  # noqa: E402
    BASEDPYRIGHT_VERSION,
    basedpyright_command,
    check,
)

ROOT = Path(__file__).resolve().parents[1]

CLEAN = """\
def double(value: int) -> int:
    return value * 2


print(double(2))
"""

# Assigning a str to an int-annotated name is an error in every mode.
WRONG_TYPE = """\
count: int = "three"
"""

# The suppression hides nothing, which reportUnnecessaryTypeIgnoreComment
# turns into an error so stale ignores cannot accumulate.
STALE_IGNORE = """\
count: int = 3  # pyright: ignore[reportAssignmentType]
"""

# Resolvable only through the uv environment the launcher hands basedpyright.
THIRD_PARTY = """\
import yaml

print(yaml.safe_dump({"ok": True}))
"""


class CommandCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="python-types-")
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        # The repository's real config, so the test cannot drift from policy.
        (self.root / "pyproject.toml").write_bytes(
            (ROOT / "pyproject.toml").read_bytes()
        )

    def run_check(self, name: str, body: str) -> subprocess.CompletedProcess[str]:
        (self.root / name).write_text(body, encoding="utf-8")
        return subprocess.run(
            basedpyright_command([name]),
            cwd=self.root,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_clean_module_passes(self) -> None:
        result = self.run_check("clean.py", CLEAN)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_type_error_fails(self) -> None:
        result = self.run_check("wrong.py", WRONG_TYPE)
        self.assertNotEqual(result.returncode, 0, "a str assigned to int must fail")
        self.assertIn("reportAssignmentType", result.stdout + result.stderr)

    def test_unnecessary_ignore_fails(self) -> None:
        result = self.run_check("stale.py", STALE_IGNORE)
        self.assertNotEqual(result.returncode, 0, "a stale ignore must fail")
        self.assertIn("reportUnnecessaryTypeIgnoreComment", result.stdout + result.stderr)

    def test_third_party_import_resolves(self) -> None:
        result = self.run_check("third_party.py", THIRD_PARTY)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class PolicyCase(unittest.TestCase):
    def test_policy_is_not_loosened(self) -> None:
        config = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
        settings = config["tool"]["basedpyright"]
        self.assertIn(settings["typeCheckingMode"], ("standard", "strict", "recommended", "all"))
        self.assertEqual(settings["reportUnnecessaryTypeIgnoreComment"], "error")
        # No rule may be switched off wholesale; fix or narrowly ignore instead.
        disabled = sorted(
            key for key, value in settings.items()
            if key.startswith("report") and value in (False, "none")
        )
        self.assertEqual(disabled, [])

    def test_version_is_pinned(self) -> None:
        self.assertRegex(BASEDPYRIGHT_VERSION, r"^basedpyright==\d+\.\d+\.\d+$")

    def test_repository_is_currently_clean(self) -> None:
        self.assertEqual(check(ROOT), 0, "every tracked .py must pass basedpyright")


if __name__ == "__main__":
    unittest.main()

"""Guard: .clud/settings.json must not pin a soldr version.

clud reconciles an explicit ``soldr_version`` on every launch by running
``uv tool install --force soldr==<pin>``, which replaces the soldr shared by
every project on the machine. An unpinned config lets clud reuse whatever
soldr is installed and install the latest release only when it is missing.
See FastLED/fbuild#1436.
"""

from __future__ import annotations

import json
import unittest
from pathlib import Path
from typing import Any

SETTINGS = Path(__file__).resolve().parent.parent / ".clud" / "settings.json"


def find_soldr_version_keys(node: Any, path: str = "") -> list[str]:
    """Return the JSON path of every ``soldr_version`` key at any depth."""
    found: list[str] = []
    if isinstance(node, dict):
        for key, value in node.items():
            child = f"{path}.{key}" if path else key
            if key == "soldr_version":
                found.append(child)
            found.extend(find_soldr_version_keys(value, child))
    elif isinstance(node, list):
        for index, value in enumerate(node):
            found.extend(find_soldr_version_keys(value, f"{path}[{index}]"))
    return found


class CludSettingsSoldrTests(unittest.TestCase):
    def setUp(self) -> None:
        self.settings = json.loads(SETTINGS.read_text(encoding="utf-8"))

    def test_soldr_version_is_not_pinned(self) -> None:
        self.assertEqual(find_soldr_version_keys(self.settings), [])

    def test_soldr_install_and_shims_stay_enabled(self) -> None:
        rust = self.settings["optimize"]["rust"]
        self.assertIs(rust["install_soldr"], True)
        self.assertIs(rust["use_soldr_shims"], True)

    def test_finder_sees_nested_pins(self) -> None:
        sample = {"a": [{"soldr_version": "1"}], "b": {"c": {"soldr_version": "2"}}}
        self.assertEqual(
            find_soldr_version_keys(sample), ["a[0].soldr_version", "b.c.soldr_version"]
        )


if __name__ == "__main__":
    unittest.main()

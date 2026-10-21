"""The public setup action's restore-only input applies to install caches too."""

import unittest
from pathlib import Path

import yaml


class SetupActionCacheTests(unittest.TestCase):
    def test_install_cache_cannot_register_an_implicit_save(self):
        action = Path(__file__).resolve().parents[1] / ".github/actions/setup/action.yml"
        steps = yaml.safe_load(action.read_text())["runs"]["steps"]
        restore = next(s for s in steps if s.get("id") == "fbuild-install-cache")
        self.assertEqual("actions/cache/restore@v5", restore["uses"])
        self.assertIn("inputs.cache == 'true'", restore["if"])
        saves = [s for s in steps if s.get("name") == "Save fbuild install cache"]
        self.assertEqual(1, len(saves))
        save = saves[0]
        self.assertEqual("actions/cache/save@v5", save["uses"])
        self.assertIn("inputs.cache == 'true'", save["if"])
        self.assertIn("inputs.save == 'true'", save["if"])
        self.assertIn("steps.fbuild-install-cache.outputs.cache-hit != 'true'", save["if"])
        self.assertEqual(restore["with"]["path"], save["with"]["path"])
        self.assertEqual("${{ steps.fbuild-install-cache.outputs.cache-primary-key }}", save["with"]["key"])
        install = next(i for i, s in enumerate(steps) if s.get("name") == "Install fbuild")
        self.assertGreater(steps.index(save), install)


if __name__ == "__main__":
    unittest.main()

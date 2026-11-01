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
        activate = next(i for i, s in enumerate(steps) if s.get("name") == "Activate fbuild from install dir")
        self.assertGreater(steps.index(save), activate)


if __name__ == "__main__":
    unittest.main()


class SetupActionRestoreKeyScopeTests(unittest.TestCase):
    """Restore keys must never cross an fbuild content hash (fbuild#1843).

    A restore that spans fbuild versions warms the cache dir with the previous
    release's packages/toolchains; the job then fetches its own and saves the
    union under the new key, so every upgrade roughly doubles the entry until
    the repository cache budget is exhausted.
    """

    def setUp(self):
        action = Path(__file__).resolve().parents[1] / ".github/actions/setup/action.yml"
        self.steps = yaml.safe_load(action.read_text())["runs"]["steps"]

    def _step(self, step_id):
        return next(s for s in self.steps if s.get("id") == step_id)

    def _restore_keys(self, step):
        return [k.strip() for k in step["with"]["restore-keys"].splitlines() if k.strip()]

    def test_combined_cache_restore_keys_never_cross_fbuild_versions(self):
        for step_id in ("fbuild-cache", "fbuild-cache-restore-only"):
            with self.subTest(step=step_id):
                step = self._step(step_id)
                keys = self._restore_keys(step)
                self.assertTrue(keys, f"{step_id} must keep a same-hash partial-hit fallback")
                for key in keys:
                    self.assertIn(
                        "steps.fbuild-hash.outputs.fbuild-hash",
                        key,
                        f"{step_id} restore key crosses fbuild versions: {key}",
                    )

    def test_split_build_payload_restore_keys_never_cross_fbuild_versions(self):
        for step_id in ("build-cache", "build-cache-restore-only"):
            with self.subTest(step=step_id):
                step = self._step(step_id)
                for key in self._restore_keys(step):
                    self.assertIn("steps.fbuild-hash.outputs.fbuild-hash", key, key)

    def test_saved_keys_and_restored_keys_share_the_hash_prefix(self):
        for step_id in ("fbuild-cache", "fbuild-cache-restore-only"):
            with self.subTest(step=step_id):
                step = self._step(step_id)
                primary = step["with"]["key"]
                self.assertIn("steps.fbuild-hash.outputs.fbuild-hash", primary)
                for key in self._restore_keys(step):
                    self.assertTrue(primary.startswith(key), f"{key} is not a prefix of {primary}")


class SetupActionInstallFreshnessTests(unittest.TestCase):
    """`fbuild-version: latest` must actually install the current release.

    The install-cache key ends in the pip spec, which for `latest` is the bare
    `fbuild` — so an exact hit is version-independent. Skipping the install on
    that hit pins every job to whichever release first populated the cache.
    """

    def setUp(self):
        action = Path(__file__).resolve().parents[1] / ".github/actions/setup/action.yml"
        self.steps = yaml.safe_load(action.read_text())["runs"]["steps"]

    def _step(self, name):
        return next(s for s in self.steps if s.get("name") == name)

    def test_latest_pip_spec_is_the_unpinned_bare_name(self):
        resolve = self._step("Resolve cache directories and fbuild install target")
        script = resolve["run"]
        self.assertIn('= "latest"', script)
        self.assertIn("pip-spec=fbuild", script)
        self.assertIn("pip-spec=fbuild==", script)

    def test_unpinned_install_is_never_skipped_on_a_cache_hit(self):
        install = self._step("Install fbuild")
        condition = install["if"]
        self.assertIn("steps.fbuild-install-cache.outputs.cache-hit != 'true'", condition)
        self.assertIn("steps.resolve-paths.outputs.pip-spec == 'fbuild'", condition)

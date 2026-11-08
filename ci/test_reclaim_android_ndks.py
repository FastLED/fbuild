"""The Linux capacity provisioner removes only SDK-owned NDK packages."""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import reclaim_android_ndks  # noqa: E402


class ReclaimAndroidNdksTest(unittest.TestCase):
    def test_main_refuses_execution_outside_hosted_linux_ci(self):
        cases = [
            ({}, "linux"),
            ({"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "self-hosted"}, "linux"),
            ({"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}, "win32"),
        ]
        for environment, platform in cases:
            with self.subTest(environment=environment, platform=platform):
                with (
                    mock.patch.dict(os.environ, environment, clear=True),
                    mock.patch.object(sys, "platform", platform),
                ):
                    with self.assertRaisesRegex(RuntimeError, "GitHub-hosted Linux"):
                        reclaim_android_ndks.main()

    def install_metadata(self, root: Path, version: str, package: str) -> None:
        metadata = root / "ndk" / version / "package.xml"
        metadata.parent.mkdir(parents=True)
        metadata.write_text(
            '<sdk:sdk-repository xmlns:sdk="http://schemas.android.com/sdk/android/repo/repository2/03">'
            f'<localPackage path="{package}"/></sdk:sdk-repository>', encoding="utf-8"
        )

    def test_only_installed_ndk_package_ids_are_selected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.install_metadata(root, "27.3", "ndk;27.3")
            self.install_metadata(root, "29.0", "ndk;29.0")
            (root / "cmake").mkdir()
            (root / "cmake" / "package.xml").write_text('<localPackage path="cmake;3.31"/>')
            self.assertEqual(["ndk;27.3", "ndk;29.0"], reclaim_android_ndks.installed_ndks(root))

    def test_missing_ndks_are_a_noop_without_sdkmanager(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertEqual([], reclaim_android_ndks.installed_ndks(root))
            reclaim_android_ndks.reclaim(root, root)

    def test_unexpected_metadata_fails_before_uninstall(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.install_metadata(root, "unexpected", "cmake;3.31")
            with self.assertRaises(ValueError):
                reclaim_android_ndks.reclaim(root, root)

    @unittest.skipUnless(os.name == "posix", "SDK manager executable fixture uses a POSIX shell")
    def test_sdkmanager_receives_exact_inventory_and_explicit_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.install_metadata(root, "27.3", "ndk;27.3")
            manager = root / "cmdline-tools" / "latest" / "bin" / "sdkmanager"
            manager.parent.mkdir(parents=True)
            manager.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$(dirname "$0")/calls"\n')
            manager.chmod(0o755)
            reclaim_android_ndks.reclaim(root, root)
            self.assertEqual(
                [f"--sdk_root={root}", "--uninstall", "ndk;27.3"],
                (manager.parent / "calls").read_text().splitlines(),
            )


if __name__ == "__main__":
    unittest.main()

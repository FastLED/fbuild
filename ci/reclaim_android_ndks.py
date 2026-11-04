"""Provision Linux CI capacity by uninstalling unused, SDK-owned Android NDKs.

These jobs build embedded/native targets, not Android. Installed package.xml
records identify packages without depending on sdkmanager's display format.
Rust/fbuild caches and their disk guards remain unchanged.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


def installed_ndks(sdk_root: Path) -> list[str]:
    """Read exact installed NDK package IDs from the SDK's own metadata."""
    packages = []
    for metadata in sorted((sdk_root / "ndk").glob("*/package.xml")):
        local = next((node for node in ET.parse(metadata).iter()
                      if node.tag.rsplit("}", 1)[-1] == "localPackage"), None)
        package = local.get("path", "") if local is not None else ""
        if not re.fullmatch(r"ndk;[0-9][A-Za-z0-9._+-]*", package):
            raise ValueError(f"Unexpected NDK package metadata: {metadata}")
        packages.append(package)
    return sorted(set(packages))


def reclaim(sdk_root: Path, volume: Path) -> None:
    """Ask the package manager to uninstall only the discovered NDK packages."""
    print(f"CI workspace free bytes before NDK provisioning: {shutil.disk_usage(volume).free}", flush=True)
    packages = installed_ndks(sdk_root)
    print(f"Unused installed Android NDK packages: {packages}", flush=True)
    if packages:
        manager = sdk_root / "cmdline-tools" / "latest" / "bin" / "sdkmanager"
        subprocess.run([str(manager), f"--sdk_root={sdk_root}", "--uninstall", *packages], check=True)
    print(f"CI workspace free bytes after NDK provisioning: {shutil.disk_usage(volume).free}", flush=True)


def main() -> None:
    """The capacity operation is restricted to hosted Linux CI runners."""
    if (os.environ.get("GITHUB_ACTIONS") != "true" or sys.platform != "linux"
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"):
        raise RuntimeError("NDK provisioning requires a GitHub-hosted Linux CI runner")
    reclaim(Path(os.environ["ANDROID_HOME"]), Path.cwd())


if __name__ == "__main__":
    main()

"""Run fbuild custom lints through soldr prebuilt Dylint tools and driver."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

from ci.env import activate

ROOT = Path(__file__).resolve().parent.parent


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", help="Check one edited file's crate instead of the workspace")
    args = parser.parse_args(argv)
    activate()
    # Soldr's rustup route needs an explicit Cargo home when the surrounding
    # Python/uv environment leaves it unset; otherwise it resolves `.cargo`
    # relative to this checkout and its post-install self-check fails.
    if not os.environ.get("CARGO_HOME"):
        os.environ["CARGO_HOME"] = str(Path.home() / ".cargo")
    env = os.environ.copy()
    # setup-soldr pins the workspace's stable toolchain in this variable.
    # Dylint must resolve the separate nightly from its library manifests.
    env.pop("RUSTUP_TOOLCHAIN", None)
    # The ordinary local check can reuse Cargo fingerprints. CI's native OS
    # legs force fresh traversal and compare the independent observation ledger.
    env.pop("FBUILD_PLATFORM_BOUNDARY_OBSERVED", None)
    subprocess.run(["soldr", "dylint", "prepare"], cwd=ROOT, env=env, check=True)
    scope = ["--workspace"] if not args.package else ["--package", args.package]
    result = subprocess.run(
        ["soldr", "dylint", "--all", "--", *scope, "--all-targets"],
        cwd=ROOT,
        env=env,
        check=False,
    )
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())

"""Run fbuild's pinned Dylint 6.0.1 checks through the global soldr front door.

The soldr 0.9.23 ``lint`` suite uses a 6.0.3 driver, while our libraries pin
6.0.1. Keep a project-local 6.0.1 executable and driver until those versions
can be migrated together. The first run bootstraps them under ``target/``.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

from ci.build_dylint_driver import DYLINT_VERSION, TOOLCHAIN_CHANNEL
from ci.env import activate

ROOT = Path(__file__).resolve().parent.parent
TOOLS_ROOT = ROOT / "target" / "dylint-tools"
DRIVER_ROOT = ROOT / "target" / "dylint-drivers"


def dylint_binary() -> Path:
    return TOOLS_ROOT / "bin" / ("cargo-dylint.exe" if os.name == "nt" else "cargo-dylint")


def command_version(binary: Path) -> str:
    try:
        return subprocess.check_output(
            ["soldr", "rustup", "run", TOOLCHAIN_CHANNEL, str(binary), "dylint", "-V"],
            cwd=ROOT,
            text=True,
            timeout=60,
        ).strip()
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return ""


def ensure_tools() -> Path:
    subprocess.run(
        [
            "soldr", "rustup", "toolchain", "install", TOOLCHAIN_CHANNEL,
            "--component", "llvm-tools-preview", "--component", "rust-src",
            "--component", "rustc-dev", "--component", "rustfmt", "--profile", "minimal",
        ],
        cwd=ROOT,
        check=True,
    )
    binary = dylint_binary()
    link = TOOLS_ROOT / "bin" / ("dylint-link.exe" if os.name == "nt" else "dylint-link")
    if command_version(binary) != f"cargo-dylint {DYLINT_VERSION}" or not link.is_file():
        subprocess.run(
            [
                "soldr", "cargo", "install", "cargo-dylint", "dylint-link",
                "--version", DYLINT_VERSION, "--locked", "--root", str(TOOLS_ROOT),
            ],
            cwd=ROOT,
            check=True,
        )
        if command_version(binary) != f"cargo-dylint {DYLINT_VERSION}":
            raise RuntimeError(f"could not install pinned cargo-dylint {DYLINT_VERSION}")
    return binary


def ensure_driver(env: dict[str, str]) -> None:
    # The helper writes both short and fully qualified channel directories.
    # A stale or different-version driver must never satisfy this check.
    host = subprocess.check_output(
        ["soldr", "rustc", "-vV"],
        cwd=ROOT,
        env={**env, "RUSTUP_TOOLCHAIN": TOOLCHAIN_CHANNEL},
        text=True,
        timeout=60,
    )
    host_triple = next(line[6:] for line in host.splitlines() if line.startswith("host: "))
    suffix = ".exe" if os.name == "nt" else ""
    driver = DRIVER_ROOT / f"{TOOLCHAIN_CHANNEL}-{host_triple}" / f"dylint-driver{suffix}"
    version = ""
    if driver.is_file():
        try:
            version = subprocess.check_output(
                [str(driver), "-V"], env={**env, "RUSTUP_TOOLCHAIN": TOOLCHAIN_CHANNEL},
                text=True, timeout=10,
            ).strip()
        except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
            pass
    if version != f"{TOOLCHAIN_CHANNEL}-{host_triple} {DYLINT_VERSION}":
        subprocess.run(
            [sys.executable, "ci/build_dylint_driver.py"],
            cwd=ROOT,
            env={**env, "RUNNER_TEMP": str(ROOT / "target")},
            check=True,
        )


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
    binary = ensure_tools()
    env = os.environ.copy()
    env["PATH"] = str(binary.parent) + os.pathsep + env.get("PATH", "")
    # On NixOS, rustc-private drivers can depend on shared libraries exposed
    # through nix-ld rather than the ordinary system linker search path.
    nix_ld = env.get("NIX_LD")
    if nix_ld:
        lib_dir = Path(nix_ld).parent
        if lib_dir.is_dir():
            env["LD_LIBRARY_PATH"] = str(lib_dir) + os.pathsep + env.get("LD_LIBRARY_PATH", "")
    env["DYLINT_DRIVER_PATH"] = str(DRIVER_ROOT)
    # The ordinary local check can reuse Cargo fingerprints. CI's native OS
    # legs force fresh traversal and compare the independent observation
    # ledger; local lint remains fast enough for the edit hook.
    env.pop("FBUILD_PLATFORM_BOUNDARY_OBSERVED", None)
    ensure_driver(env)
    scope = ["--workspace"] if not args.package else ["--package", args.package]
    result = subprocess.run(
        [
            "soldr", "rustup", "run", TOOLCHAIN_CHANNEL, str(binary),
            "dylint", "--all", "--", *scope, "--all-targets",
        ],
        cwd=ROOT,
        env=env,
        check=False,
    )
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())

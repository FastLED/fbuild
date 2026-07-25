"""Build the published Dylint driver with its toolchain environment intact.

Dylint 6.0.1's driver builder clears ``RUSTUP_TOOLCHAIN`` before compiling
``dylint_driver``, while that crate's build script requires the variable
(trailofbits/dylint#1172 tracks the same missing-environment failure class).
Build the published crate once and export its standard driver directory.

Unlike the pre-6.0 workaround, this does not clone Dylint or select a git
revision. Remove it when the published driver builder propagates its channel.
"""

from __future__ import annotations

import os
import shutil
import signal
import subprocess
import sys
import tempfile
from pathlib import Path

DYLINT_VERSION = "6.0.1"
TOOLCHAIN_CHANNEL = "nightly-2026-04-16"


def run(args: list[str], **kwargs) -> subprocess.CompletedProcess[str]:  # noqa: ANN003
    """Run a bounded subprocess and terminate its process group on timeout."""
    print("+", " ".join(args), flush=True)
    timeout = kwargs.pop("timeout", 600)
    if os.name == "nt":
        kwargs.setdefault("creationflags", subprocess.CREATE_NEW_PROCESS_GROUP)
    else:
        kwargs.setdefault("start_new_session", True)
    process = subprocess.Popen(args, text=True, **kwargs)
    try:
        returncode = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        print(
            f"::error::command exceeded {timeout}s; terminating its process group",
            flush=True,
        )
        if os.name == "nt":
            subprocess.run(
                ["taskkill", "/PID", str(process.pid), "/T", "/F"], check=False
            )
        else:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
        process.wait()
        raise
    if returncode:
        raise subprocess.CalledProcessError(returncode, args)
    return subprocess.CompletedProcess(args, returncode)


def rustc_host() -> str:
    """Return the host triple for the pinned nightly."""
    env = os.environ.copy()
    env["RUSTUP_TOOLCHAIN"] = TOOLCHAIN_CHANNEL
    output = subprocess.check_output(
        ["soldr", "rustc", "-vV"],
        env=env,
        text=True,
        timeout=60,
    )
    for line in output.splitlines():
        if line.startswith("host: "):
            return line.split("host: ", 1)[1]
    raise RuntimeError("could not determine rustc host triple")


def rustc_toolchain_root(full_toolchain: str) -> Path:
    """Return the selected nightly's installation root."""
    rustc = subprocess.check_output(
        [
            "soldr",
            "rustup",
            "which",
            "--toolchain",
            full_toolchain,
            "rustc",
        ],
        text=True,
        timeout=60,
    ).strip()
    return Path(rustc).resolve().parent.parent


def write_driver_package(package: Path, full_toolchain: str) -> None:
    """Write a minimal binary package around the published driver crate."""
    src = package / "src"
    src.mkdir(parents=True)
    (package / "Cargo.toml").write_text(
        f"""
[package]
name = "dylint_driver-{full_toolchain}"
version = "0.1.0"
edition = "2021"

[dependencies]
anyhow = "1"
env_logger = "0.11"
dylint_driver = "={DYLINT_VERSION}"
""".lstrip(),
        encoding="utf-8",
    )
    (package / "rust-toolchain.toml").write_text(
        f"""
[toolchain]
channel = "{full_toolchain}"
components = ["llvm-tools-preview", "rustc-dev"]
""".lstrip(),
        encoding="utf-8",
    )
    (src / "main.rs").write_text(
        """
#![feature(rustc_private)]

use anyhow::Result;
use std::env;

fn main() -> Result<()> {
    env_logger::init();
    if env::var_os("RUSTUP_TOOLCHAIN").is_none() {
        // Dylint's runner sanitizes this variable before invoking the driver,
        // but dylint_driver uses it at runtime to locate the nightly sysroot.
        // SAFETY: this is the single-threaded driver entry point.
        unsafe {
            env::set_var("RUSTUP_TOOLCHAIN", env!("RUSTUP_TOOLCHAIN"));
        }
    }
    let args: Vec<_> = env::args_os().collect();
    dylint_driver::dylint_driver(&args)
}
""".lstrip(),
        encoding="utf-8",
    )


def append_github_env(name: str, value: Path) -> None:
    github_env = os.environ.get("GITHUB_ENV")
    if github_env:
        with open(github_env, "a", encoding="utf-8") as file:
            file.write(f"{name}={value}\n")


def find_built_driver(target: Path, full_toolchain: str) -> Path:
    suffix = ".exe" if os.name == "nt" else ""
    expected = f"dylint_driver-{full_toolchain}{suffix}"
    candidates = [
        path
        for path in target.rglob(expected)
        if path.is_file() and path.parent.name != "deps"
    ]
    if len(candidates) != 1:
        rendered = ", ".join(str(path) for path in candidates) or "none"
        raise RuntimeError(f"expected one built Dylint driver, found: {rendered}")
    return candidates[0]


def main() -> int:
    full_toolchain = f"{TOOLCHAIN_CHANNEL}-{rustc_host()}"
    runner_temp = Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir())).resolve()
    driver_root = runner_temp / "dylint-drivers"
    driver_dirs = {
        driver_root / TOOLCHAIN_CHANNEL,
        driver_root / full_toolchain,
    }
    for driver_dir in driver_dirs:
        driver_dir.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="fbuild-dylint-") as temp:
        package = Path(temp) / "driver-package"
        package.mkdir()
        write_driver_package(package, full_toolchain)

        env = os.environ.copy()
        env["RUSTUP_TOOLCHAIN"] = full_toolchain
        env["CARGO_TARGET_DIR"] = str(package / "target")
        if os.name != "nt":
            rpath = f"-C link-args=-Wl,-rpath,{rustc_toolchain_root(full_toolchain) / 'lib'}"
            env["RUSTFLAGS"] = f"{env.get('RUSTFLAGS', '')} {rpath}".strip()
        run(
            ["soldr", "--no-cache", "cargo", "build"],
            cwd=package,
            env=env,
        )

        built_driver = find_built_driver(package / "target", full_toolchain)
        suffix = ".exe" if os.name == "nt" else ""
        for driver_dir in driver_dirs:
            # Dylint 6.0.1 probes the extensionless path even on Windows.
            shutil.copy2(built_driver, driver_dir / "dylint-driver")
            if suffix:
                shutil.copy2(built_driver, driver_dir / f"dylint-driver{suffix}")

    append_github_env("DYLINT_DRIVER_PATH", driver_root)
    print(f"DYLINT_DRIVER_PATH={driver_root}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

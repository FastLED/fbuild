"""Build a Dylint driver from the git revision used by the lint crate.

The published `dylint_driver` 5.0.0 crate does not build against the
nightly toolchain pinned by CI. The lint crate already pins Dylint's git
revision for `dylint_linting` and `dylint_testing`; this script builds the
matching driver from that checkout and exports DYLINT_DRIVER_PATH.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


DYLINT_REPO = "https://github.com/trailofbits/dylint"
DYLINT_REV = "4bd91ce7729b74c7ee5664bbb588f7baf30b4a09"
TOOLCHAIN_CHANNEL = "nightly-2026-03-26"


def run(args: list[str], **kwargs) -> subprocess.CompletedProcess[str]:  # noqa: ANN003
    print("+", " ".join(args), flush=True)
    # FastLED/fbuild#812: every subprocess in this CI script gets a
    # watchdog so a stuck `git clone` / cargo build can't burn the GHA
    # job's full 6h default. Per-call timeout can be overridden via
    # kwargs (e.g. cargo builds use 600s).
    kwargs.setdefault("timeout", 600)
    return subprocess.run(args, check=True, text=True, **kwargs)


def rustc_host() -> str:
    # Invoke rustc through `rustup run` so the call works even when PATH
    # is fronted by shims (e.g. soldr) that do not understand the
    # `+<toolchain>` directive that only the rustup `cargo`/`rustc`
    # wrappers parse.
    output = subprocess.check_output(
        ["rustup", "run", TOOLCHAIN_CHANNEL, "rustc", "-vV"],
        text=True,
        timeout=60,
    )
    for line in output.splitlines():
        if line.startswith("host: "):
            return line.split("host: ", 1)[1]
    raise RuntimeError("could not determine rustc host triple")


def rustc_toolchain_root(full_toolchain: str) -> Path:
    rustc = subprocess.check_output(
        ["rustup", "which", "--toolchain", full_toolchain, "rustc"],
        text=True,
        timeout=30,
    ).strip()
    return Path(rustc).resolve().parent.parent


def write_driver_package(package: Path, dylint_checkout: Path, full_toolchain: str) -> None:
    src = package / "src"
    src.mkdir(parents=True)

    driver_path = str((dylint_checkout / "driver").resolve()).replace("\\", "\\\\")
    (package / "Cargo.toml").write_text(
        f"""
[package]
name = "dylint_driver-{full_toolchain}"
version = "0.1.0"
edition = "2018"

[dependencies]
anyhow = "1.0"
env_logger = "0.11"
dylint_driver = {{ path = "{driver_path}" }}
""".lstrip(),
        encoding="utf-8",
    )
    # Use `.toml` extension so rustup unambiguously parses as TOML.
    # The extensionless `rust-toolchain` form is ambiguous (single-line
    # vs TOML) and on Windows hosts has been observed to silently fall
    # through to the default toolchain, leaving the build script's
    # `#![feature(...)]` rejected as "stable channel".
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

pub fn main() -> Result<()> {
    env_logger::init();

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


def main() -> int:
    full_toolchain = f"{TOOLCHAIN_CHANNEL}-{rustc_host()}"
    runner_temp = Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir())).resolve()
    driver_root = runner_temp / "dylint-drivers"
    driver_dir = driver_root / full_toolchain
    driver_dir.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="fbuild-dylint-") as temp:
        temp_path = Path(temp)
        checkout = temp_path / "dylint"
        package = temp_path / "driver-package"

        run(["git", "clone", "--filter=blob:none", DYLINT_REPO, str(checkout)])
        run(["git", "-C", str(checkout), "checkout", DYLINT_REV])

        package.mkdir()
        write_driver_package(package, checkout, full_toolchain)

        env = os.environ.copy()
        # Force the rustup toolchain in the env so it propagates into
        # nested cargo/rustc invocations (e.g. build-script compilation
        # of dylint_driver which uses `#![feature(...)]` and requires
        # nightly). Setting via env is more reliable than relying solely
        # on the `rust-toolchain.toml` lookup, especially on Windows.
        env["RUSTUP_TOOLCHAIN"] = full_toolchain
        # Anchor RUSTC and CARGO to the specific nightly binaries to
        # defeat shadowing by any stable `rustc`/`cargo` that may appear
        # earlier in PATH (e.g. a Chocolatey-installed stable on
        # Windows). Without this, cargo's build-script rustc invocation
        # may resolve to a stable rustc and fail on `#![feature(...)]`
        # with E0554.
        nightly_bin = rustc_toolchain_root(full_toolchain) / "bin"
        rustc_exe = nightly_bin / ("rustc.exe" if os.name == "nt" else "rustc")
        cargo_exe = nightly_bin / ("cargo.exe" if os.name == "nt" else "cargo")
        if rustc_exe.exists():
            env["RUSTC"] = str(rustc_exe)
        if cargo_exe.exists():
            env["CARGO"] = str(cargo_exe)
        if os.name != "nt":
            toolchain_root = rustc_toolchain_root(full_toolchain)
            rpath = f"-C link-args=-Wl,-rpath,{toolchain_root / 'lib'}"
            env["RUSTFLAGS"] = f"{env.get('RUSTFLAGS', '')} {rpath}".strip()

        # Use `rustup run` instead of `cargo +<toolchain>` because the
        # cargo on PATH may be a shim (e.g. soldr's) that does not parse
        # the `+<toolchain>` directive — that directive is only honored
        # by the rustup-managed cargo wrapper. `rustup run` selects the
        # toolchain explicitly and works regardless of which `cargo`
        # comes first on PATH.
        run(
            ["rustup", "run", TOOLCHAIN_CHANNEL, "cargo", "build"],
            cwd=package,
            env=env,
        )

        exe_suffix = ".exe" if os.name == "nt" else ""
        built_driver = package / "target" / "debug" / f"dylint_driver-{full_toolchain}{exe_suffix}"
        installed_driver = driver_dir / f"dylint-driver{exe_suffix}"
        shutil.copy2(built_driver, installed_driver)

    append_github_env("DYLINT_DRIVER_PATH", driver_root)
    print(f"DYLINT_DRIVER_PATH={driver_root}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

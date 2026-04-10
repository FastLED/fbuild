"""Centralized PATH activation for Rust toolchain.

Ensures .cargo/bin is on PATH before invoking Rust tools.
Import and call activate() at the top of CI scripts.
"""

import os
import shutil
import subprocess


def find_cargo_bin():
    """Locate .cargo/bin cross-platform.

    Checks CARGO_HOME, then ~/.cargo, then %USERPROFILE%\\.cargo.
    Returns the absolute path to the bin directory, or None.
    """
    candidates = [
        os.environ.get("CARGO_HOME", ""),
        os.path.join(os.path.expanduser("~"), ".cargo"),
    ]
    userprofile = os.environ.get("USERPROFILE", "")
    if userprofile:
        candidates.append(os.path.join(userprofile, ".cargo"))

    for candidate in candidates:
        if candidate:
            bin_dir = os.path.join(candidate, "bin")
            if os.path.isdir(bin_dir):
                return os.path.abspath(bin_dir)

    rustup = shutil.which("rustup")
    if rustup:
        try:
            tool_path = subprocess.check_output(
                [rustup, "which", "cargo"],
                text=True,
                stderr=subprocess.DEVNULL,
            ).strip()
            if tool_path and os.path.isfile(tool_path):
                return os.path.abspath(os.path.dirname(tool_path))
        except Exception:
            pass

    return None


def activate():
    """Prepend .cargo/bin to PATH if not already present.

    Call this at the top of any CI script that invokes Rust tools.
    """
    cargo_bin = find_cargo_bin()
    if not cargo_bin:
        return
    current_path = os.environ.get("PATH", "")
    if cargo_bin not in current_path.split(os.pathsep):
        os.environ["PATH"] = cargo_bin + os.pathsep + current_path


def clean_env():
    """Return an env dict with .cargo/bin on PATH and VIRTUAL_ENV removed.

    Useful for subprocess calls where venv interference with Rust builds
    should be avoided.
    """
    env = os.environ.copy()

    # Ensure .cargo/bin is on PATH
    cargo_bin = find_cargo_bin()
    if cargo_bin:
        path_parts = env.get("PATH", "").split(os.pathsep)
        if cargo_bin not in path_parts:
            env["PATH"] = cargo_bin + os.pathsep + env.get("PATH", "")

    # Remove VIRTUAL_ENV to avoid venv interference
    env.pop("VIRTUAL_ENV", None)

    return env

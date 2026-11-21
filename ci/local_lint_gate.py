"""Run the repository's full RUST-032 lint phase as one local-gate lane.

``soldr lint rust --cross-dylint`` runs formatting validation, Clippy for the
host and every target in ``[workspace.metadata.soldr].targets`` (soldr#3378),
and Dylint for every selected target using the lint libraries' pinned nightly.
The repository's remote lint scope is ``soldr cargo clippy --workspace
--all-targets -- -D warnings`` (check-ubuntu.yml "Clippy"); soldr applies the
workspace scope and denies warnings itself, so no extra cargo flags are needed.
Passing ``--all-targets`` through makes cargo reject the invocation as a
duplicate.

Cross targets lint from this Linux host through soldr's prebuilt ``rust-std``,
so the lane never needs a native Windows or macOS machine. A non-Linux host is
a hard error rather than a silent reduction in the attested coverage.

FastLED/fbuild#1793 (fleet policy RUST-033).
"""

from __future__ import annotations

import os
import platform
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def lint_command() -> list[str]:
    """The canonical RUST-033 command: full lint phase, Dylint per target."""

    return ["soldr", "lint", "rust", "--cross-dylint"]


def run_lint() -> None:
    host = platform.system()
    if host != "Linux":
        print(
            f"lint lane: unsupported host {host!r}; the RUST-032 lint phase "
            "lints cross targets from a Linux host only",
            file=sys.stderr,
        )
        raise SystemExit(1)
    command = lint_command()
    print("lint lane: " + " ".join(command), flush=True)
    result = subprocess.run(command, cwd=ROOT, check=False)
    if result.returncode != 0:
        print(f"lint lane: failed with exit code {result.returncode}", file=sys.stderr)
    raise SystemExit(result.returncode)


def run_cross_clippy() -> None:
    """The Ubuntu replay checks host Clippy; this lane checks format and cross Clippy."""
    if platform.system() != "Linux" or platform.machine().lower() not in {"x86_64", "amd64"}:
        raise SystemExit("cross Clippy lane requires a Linux x86-64 host")
    from ci.local_gate import load_aware_test_threads

    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    targets = manifest["workspace"]["metadata"]["soldr"]["targets"]
    threads = load_aware_test_threads(len(os.sched_getaffinity(0)), os.getloadavg()[0])
    env = {**os.environ, "CARGO_BUILD_JOBS": str(threads)}
    # These checks type-check abi3 targets without linking or running the
    # embedded Python tests. The Ubuntu lane runs those with real CPython.
    cross_env = {**env, "PYO3_CROSS": "1", "PYO3_CROSS_PYTHON_VERSION": "3.10"}
    subprocess.run(["soldr", "cargo", "fmt", "--all", "--check"], cwd=ROOT, env=env, check=True)
    for target in targets:
        if target == "x86_64-unknown-linux-gnu":
            continue
        subprocess.run(
            ["soldr", "cargo", "clippy", "--workspace", "--all-targets", "--target", target, "--", "-D", "warnings"],
            cwd=ROOT, env=cross_env, check=True,
        )


if __name__ == "__main__":
    if sys.argv[1:] == ["--cross-clippy"]:
        run_cross_clippy()
    else:
        run_lint()

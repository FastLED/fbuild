"""Run the Dylint local-gate lane on the host: policy validators, lint libraries, ``soldr lint``.

Lint never runs under bosn; ``soldr lint`` is the only local linter. The lane
used to replay ``dylint.yml`` through a bosn/act container, whose caching
semantics break the shared build host. It now runs the same checks the hosted
``dylint.yml`` jobs run, directly on the host:

1. The lightweight Python policy validators (allowlist paths, the shrink-only
   ``.fbuild`` baseline, the platform-boundary ledgers) through ``uv``.
2. Each lint library's formatting and UI tests through ``soldr cargo``. Every
   ``dylints/*`` crate pins its own nightly in ``rust-toolchain.toml``, so the
   commands run from the crate directory with ``RUSTUP_TOOLCHAIN`` removed.
3. ``soldr lint rust --check --cross-dylint``: formatting, Clippy and every
   ``[workspace.metadata.dylint]`` library over the whole workspace, for the
   host and every ``[workspace.metadata.soldr].targets`` triple.

The hosted job's extra comparison of raw Dylint observations against the
platform-boundary ledger stays remote: it needs a fresh, uncached driver pass,
which a warm local cache deliberately does not do.
"""

from __future__ import annotations

import os
import platform
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
UV_PYTHON = ("uv", "run", "--no-project", "python")
VALIDATORS: tuple[tuple[str, ...], ...] = (
    ("ci/check_dylint_allowlists.py",),
    ("ci/check_fbuild_path_baseline.py", "--base", "origin/main"),
    ("ci/enforce_platform_boundary.py", "--print-totals"),
)


def lint_libraries(root: Path = ROOT) -> list[Path]:
    """Every lint library crate under ``dylints/``, in a stable order."""
    return sorted(path.parent for path in root.glob("dylints/*/Cargo.toml"))


def lint_command() -> list[str]:
    """The workspace lint: fmt, Clippy and Dylint for the host and every target."""
    return ["soldr", "lint", "rust", "--check", "--cross-dylint"]


def host_commands(root: Path = ROOT) -> list[tuple[list[str], Path]]:
    """Every command the lane runs, with its working directory, in order."""
    commands: list[tuple[list[str], Path]] = [
        ([*UV_PYTHON, *validator], root) for validator in VALIDATORS
    ]
    commands.append((["soldr", "dylint", "prepare"], root))
    libraries = lint_libraries(root)
    commands.extend(
        (["soldr", "cargo", "fmt", "--all", "--", "--check"], library)
        for library in libraries
    )
    commands.extend((["soldr", "cargo", "test"], library) for library in libraries)
    commands.append((lint_command(), root))
    return commands


def lane_env(root: Path = ROOT) -> dict[str, str]:
    env = os.environ.copy()
    # The stable workspace pin must not override each library's nightly pin.
    env.pop("RUSTUP_TOOLCHAIN", None)
    env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    # Library UI tests share one target tree, apart from the workspace build.
    env["CARGO_TARGET_DIR"] = str(root / "target" / "dylint-tests")
    return env


def run_dylint(root: Path = ROOT) -> None:
    if platform.system() != "Linux":
        raise SystemExit("dylint lane: cross-target Dylint runs from a Linux host only")
    env = lane_env(root)
    for argv, cwd in host_commands(root):
        print(f"dylint lane: ({cwd.relative_to(root)}) {' '.join(argv)}", flush=True)
        # The workspace lint keeps soldr's own target directory.
        command_env = env if argv != lint_command() else {
            key: value for key, value in env.items() if key != "CARGO_TARGET_DIR"
        }
        result = subprocess.run(argv, cwd=cwd, env=command_env, check=False)
        if result.returncode != 0:
            print(f"dylint lane: failed with exit code {result.returncode}", file=sys.stderr)
            raise SystemExit(result.returncode)


if __name__ == "__main__":
    run_dylint()

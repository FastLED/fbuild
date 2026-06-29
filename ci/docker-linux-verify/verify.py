"""Local Linux verification harness — reproduces `check-ubuntu.yml`
locally on a Windows host using the existing `fbuild-mac-cross` Docker
image, with named volumes for fast incremental rebuilds.

The host bind-mount carries the repo source; cargo's `target/` and
`CARGO_HOME` live in **named Docker volumes** rather than host paths
because Windows-host WSL2 9P translation rewrites mtimes per container
start, defeating cargo's incremental fingerprint check. Named volumes
sit on Linux-native ext4 inside Docker's VFS and keep no-op rebuilds
in single-digit seconds.

Usage::

    uv run python ci/docker-linux-verify/verify.py                # full check + clippy + test
    uv run python ci/docker-linux-verify/verify.py --shell        # interactive bash in the image
    uv run python ci/docker-linux-verify/verify.py --wipe         # remove named volumes (force cold rebuild)

This is the local-loop equivalent of pushing a branch and waiting for
GHA's Check Ubuntu lane. First run is a full cold build (~5-8 min on a
fast machine). Subsequent runs after a source edit are seconds-to-minutes.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
IMAGE = "fbuild-mac-cross"
VOLUME_TARGET = "fbuild-linux-verify-target"
VOLUME_CARGO_HOME = "fbuild-linux-verify-cargo-home"


def _run(cmd: list[str], *, check: bool = True) -> int:
    print(f"$ {' '.join(cmd)}", flush=True)
    result = subprocess.run(cmd, check=False)
    if check and result.returncode != 0:
        sys.exit(result.returncode)
    return result.returncode


def _ensure_image() -> None:
    rc = subprocess.run(
        ["docker", "image", "inspect", IMAGE],
        capture_output=True,
    ).returncode
    if rc == 0:
        return
    print(f"image {IMAGE} not found — building from ci/docker-mac-cross/", flush=True)
    _run(
        [
            "docker",
            "build",
            "-f",
            str(REPO_ROOT / "ci" / "docker-mac-cross" / "Dockerfile"),
            "-t",
            IMAGE,
            str(REPO_ROOT),
        ]
    )


def _wipe_volumes() -> None:
    for vol in (VOLUME_TARGET, VOLUME_CARGO_HOME):
        rc = _run(["docker", "volume", "rm", vol], check=False)
        if rc != 0:
            print(f"  (volume {vol} did not exist — skipping)", flush=True)


def _docker_run(args: list[str], *, interactive: bool = False) -> int:
    src_mount = str(REPO_ROOT).replace("\\", "/")
    run_args = [
        "docker",
        "run",
        "--rm",
        "-v",
        f"{src_mount}:/src",
        "-v",
        f"{VOLUME_TARGET}:/target",
        "-v",
        f"{VOLUME_CARGO_HOME}:/cargo-home",
        "-w",
        "/src",
    ]
    if interactive:
        run_args += ["-it"]
    run_args += [IMAGE] + args
    return _run(run_args, check=False)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--shell",
        action="store_true",
        help="drop into an interactive bash inside the image (skip verify.sh)",
    )
    p.add_argument(
        "--wipe",
        action="store_true",
        help="remove the named cargo volumes (force a cold rebuild next time)",
    )
    p.add_argument(
        "--rebuild-image",
        action="store_true",
        help=f"force a `docker build` of the {IMAGE} image before verifying",
    )
    args = p.parse_args()

    if args.wipe:
        _wipe_volumes()
        if not (args.shell or args.rebuild_image):
            return 0

    if args.rebuild_image:
        _run(
            [
                "docker",
                "build",
                "--no-cache",
                "-f",
                str(REPO_ROOT / "ci" / "docker-mac-cross" / "Dockerfile"),
                "-t",
                IMAGE,
                str(REPO_ROOT),
            ]
        )
    else:
        _ensure_image()

    if args.shell:
        return _docker_run(["bash"], interactive=True)

    return _docker_run(["bash", "ci/docker-linux-verify/verify.sh"])


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Host orchestrator for the fbuild Docker profiling harness (FastLED/fbuild#942).

Builds the fbuild-profile-linux image, wires up the named volumes that
keep the *harness* fast across runs (cargo/rustup/soldr homes and the
fbuild target dir), and runs the profiled build scenarios in a
privileged Linux container. The system under test's own cache
(~/.fbuild inside the container) is deliberately NOT on any volume, so
every container start is a genuinely cold fbuild cache.

Usage (from repo root):
    uv run python ci/docker-profile/run_profile.py                 # cold+warm+hot, 3 iters
    uv run python ci/docker-profile/run_profile.py --iterations 1 --scenarios cold
    uv run python ci/docker-profile/run_profile.py --wipe          # drop harness volumes
    uv run python ci/docker-profile/run_profile.py --status        # show volumes

Artifacts land in ci/docker-profile/out/<timestamp>/ (gitignored):
timings.jsonl, per-run flamegraphs (oncpu.svg / offcpu.svg), folded
stacks, daemon logs, FBUILD_PERF_LOG phase lines, and summary.md.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import statistics
import subprocess
import sys
from pathlib import Path

IMAGE = "fbuild-profile-linux"
REPO_ROOT = Path(__file__).resolve().parent.parent.parent
HARNESS_DIR = Path(__file__).resolve().parent

# Named volumes: build-state caches for the HARNESS ONLY (never the
# fbuild cache under test). Host bind mounts are not an option for
# these on Windows/WSL2 — the 9P layer rewrites mtimes on every
# container start, busting cargo's fingerprints and forcing full
# rebuilds of fbuild itself.
VOLUMES = {
    "fbuild-profile-target": "/work/target",
    "fbuild-profile-cargo-home": "/root/.cargo",
    "fbuild-profile-rustup-home": "/root/.rustup",
    "fbuild-profile-soldr-home": "/root/.soldr",
}


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    print(f"$ {' '.join(cmd)}", flush=True)
    return subprocess.run(cmd, **kw)


def docker_env() -> dict[str, str]:
    env = os.environ.copy()
    # Stop Git-Bash from rewriting /work into a Windows path before
    # docker sees it.
    env.setdefault("MSYS_NO_PATHCONV", "1")
    return env


def build_image(rebuild: bool) -> None:
    cmd = ["docker", "build", "-f", str(HARNESS_DIR / "Dockerfile"), "-t", IMAGE]
    if rebuild:
        cmd.append("--no-cache")
    cmd.append(str(HARNESS_DIR))
    rc = run(cmd, env=docker_env()).returncode
    if rc != 0:
        sys.exit(rc)


def ensure_volumes() -> None:
    for name in VOLUMES:
        run(["docker", "volume", "create", name], env=docker_env(),
            stdout=subprocess.DEVNULL)


def wipe_volumes() -> None:
    for name in VOLUMES:
        run(["docker", "volume", "rm", "--force", name], env=docker_env())


def show_status() -> None:
    for name, mount in VOLUMES.items():
        r = run(["docker", "volume", "inspect", name, "--format",
                 "{{.Name}} -> {{.Mountpoint}}"],
                env=docker_env(), capture_output=True, text=True)
        state = r.stdout.strip() if r.returncode == 0 else "(absent)"
        print(f"  {name} @ {mount}: {state}")


def run_container(out_dir: Path, iterations: int, scenarios: str,
                  env_name: str) -> int:
    cmd = [
        "docker", "run", "--rm", "--init",
        # perf_event_open + BPF need privileged on Docker Desktop/WSL2.
        "--privileged",
        "-v", f"{REPO_ROOT}:/work",
        "-v", f"{out_dir}:/out",
    ]
    for name, mount in VOLUMES.items():
        cmd += ["-v", f"{name}:{mount}"]
    cmd += [
        "-e", f"FBUILD_PROFILE_ITERS={iterations}",
        "-e", f"FBUILD_PROFILE_SCENARIOS={scenarios}",
        "-e", f"FBUILD_PROFILE_ENV={env_name}",
        "-w", "/work",
        IMAGE,
        "bash", "ci/docker-profile/profile_entry.sh",
    ]
    # No -t: mintty/Git-Bash fools isatty() and docker then errors with
    # "the input device is not a TTY".
    return run(cmd, env=docker_env()).returncode


def summarize(out_dir: Path) -> None:
    timings = out_dir / "timings.jsonl"
    if not timings.exists():
        print("no timings.jsonl produced — container failed early?")
        return
    by_scenario: dict[str, list[float]] = {}
    failures: dict[str, int] = {}
    for line in timings.read_text().splitlines():
        if not line.strip():
            continue
        rec = json.loads(line)
        by_scenario.setdefault(rec["scenario"], []).append(rec["wall_s"])
        if rec["exit"] != 0:
            failures[rec["scenario"]] = failures.get(rec["scenario"], 0) + 1

    lines = ["# fbuild profile summary", ""]
    meta = out_dir / "meta.json"
    if meta.exists():
        lines += [f"```json\n{meta.read_text().strip()}\n```", ""]
    lines += ["| scenario | runs | median s | min s | max s | failures |",
              "|---|---|---|---|---|---|"]
    for scenario, walls in by_scenario.items():
        lines.append(
            f"| {scenario} | {len(walls)} | {statistics.median(walls):.1f} "
            f"| {min(walls):.1f} | {max(walls):.1f} "
            f"| {failures.get(scenario, 0)} |")
    text = "\n".join(lines) + "\n"
    (out_dir / "summary.md").write_text(text)
    print(text)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--iterations", "-n", type=int, default=3)
    ap.add_argument("--scenarios", default="cold,warm,hot",
                    help="comma list of cold,warm,hot")
    ap.add_argument("--env", dest="env_name", default="demo",
                    help="platformio env to build (default: demo)")
    ap.add_argument("--out", type=Path, default=None,
                    help="artifact dir (default ci/docker-profile/out/<ts>)")
    ap.add_argument("--rebuild-image", action="store_true",
                    help="docker build --no-cache")
    ap.add_argument("--skip-image", action="store_true",
                    help="reuse existing image without rebuilding layers")
    ap.add_argument("--wipe", action="store_true",
                    help="remove harness volumes and exit")
    ap.add_argument("--status", action="store_true",
                    help="show harness volume state and exit")
    args = ap.parse_args()

    if args.wipe:
        wipe_volumes()
        return 0
    if args.status:
        show_status()
        return 0

    out_dir = args.out or (HARNESS_DIR / "out" /
                           _dt.datetime.now().strftime("%Y%m%d-%H%M%S"))
    out_dir.mkdir(parents=True, exist_ok=True)

    if not args.skip_image:
        build_image(args.rebuild_image)
    ensure_volumes()
    rc = run_container(out_dir.resolve(), args.iterations, args.scenarios,
                       args.env_name)
    summarize(out_dir)
    print(f"artifacts: {out_dir}")
    return rc


if __name__ == "__main__":
    sys.exit(main())

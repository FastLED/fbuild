#!/usr/bin/env python3
"""Bench uv run / uv sync wall-clock cost across realistic dev-loop edits.

Scenarios:
  warm                  - uv sync + uv run with venv already correct
  rust_touched          - touch a .rs file, then uv sync + uv run
  python_touched        - touch a python shim, then uv sync + uv run
  both_touched          - touch both, then uv sync + uv run
  forced_reinstall_clean - uv sync --reinstall-package fbuild with no
                          prior source touches (mtime-skip should fire)
  forced_reinstall_dirty - same but after touching a .rs file (cargo
                           incremental rebuild)

Run as:
    python ci/bench_uv_run.py <label>

Output: ci/bench-results/<label>.json
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
RESULTS_DIR = REPO_ROOT / "ci" / "bench-results"

RUST_FILE = REPO_ROOT / "crates" / "fbuild-core" / "src" / "lib.rs"
PYTHON_FILE = REPO_ROOT / "python" / "fbuild" / "__init__.py"

UV_RUN_NOOP = ["uv", "run", "python", "-c", "pass"]
UV_SYNC = ["uv", "sync"]
UV_REINSTALL = ["uv", "sync", "--reinstall-package", "fbuild"]


def time_command(cmd: list[str]) -> tuple[float, int]:
    # Bypass the soldr#805 hook that intercepts `uv run` / `uv sync` on
    # hybrid Python+Rust projects. The hook is good guidance for humans
    # but blocks the actual measurement we're trying to take here.
    # FastLED/fbuild#812: 10-minute per-invocation cap so a wedged
    # `uv sync --reinstall-package fbuild` (which compiles fbuild from
    # scratch) can't quietly hang the bench harness.
    start = time.perf_counter()
    proc = subprocess.run(
        cmd,
        cwd=str(REPO_ROOT),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=_bench_env(),
        timeout=600,
    )
    elapsed = time.perf_counter() - start
    return elapsed, proc.returncode


def touch(path: Path) -> None:
    if not path.is_file():
        raise FileNotFoundError(f"benchmark target does not exist: {path}")
    new_mtime = max(path.stat().st_mtime, time.time()) + 1
    os.utime(path, (new_mtime, new_mtime))


def _bench_env() -> dict:
    return {**os.environ, "CLUD_UV_RUST_ALLOW_ALL": "1"}


def warm_state() -> None:
    # FastLED/fbuild#812: 10-minute cap on the pre-warm uv sync.
    subprocess.run(UV_SYNC, cwd=str(REPO_ROOT), check=False,
                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                   env=_bench_env(),
                   timeout=600)


def force_rebuild() -> None:
    # FastLED/fbuild#812: 10-minute cap on the reinstall (rebuilds fbuild
    # from source, which is the dominant time in a cold scenario).
    subprocess.run(UV_REINSTALL, cwd=str(REPO_ROOT), check=False,
                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                   env=_bench_env(),
                   timeout=600)


def measure_scenario(name: str, before: callable, cmds: list[tuple[str, list[str]]]) -> dict:
    print(f"\n=== {name} ===", flush=True)
    before()
    out: dict = {"commands": []}
    for label, cmd in cmds:
        elapsed, rc = time_command(cmd)
        print(f"  [{label}] {' '.join(cmd)}: {elapsed:.3f}s (rc={rc})", flush=True)
        out["commands"].append({"label": label, "cmd": cmd, "elapsed_s": elapsed, "returncode": rc})
    return out


def main() -> int:
    label = sys.argv[1] if len(sys.argv) > 1 else "unlabeled"
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    out_path = RESULTS_DIR / f"{label}.json"

    if not RUST_FILE.is_file() or not PYTHON_FILE.is_file():
        print(f"missing target files: {RUST_FILE} or {PYTHON_FILE}", file=sys.stderr)
        return 1

    results: dict[str, object] = {
        "label": label,
        "platform": sys.platform,
        "python": sys.version.split()[0],
        "rust_file": str(RUST_FILE.relative_to(REPO_ROOT)),
        "python_file": str(PYTHON_FILE.relative_to(REPO_ROOT)),
        "scenarios": {},
    }

    print("[setup] pre-warm uv sync", flush=True)
    warm_state()

    # Pre-prime cargo's incremental cache so the "dirty" measurement
    # reflects the realistic warm-cache state a developer sees.
    print("[setup] pre-prime cargo via forced reinstall", flush=True)
    force_rebuild()

    results["scenarios"]["warm"] = measure_scenario(
        "warm (no touch)",
        before=lambda: None,
        cmds=[
            ("uv_run_noop", UV_RUN_NOOP),
            ("uv_sync", UV_SYNC),
        ],
    )

    results["scenarios"]["rust_touched"] = measure_scenario(
        "rust touched (uv sync only checks if rebuild needed)",
        before=lambda: touch(RUST_FILE),
        cmds=[
            ("uv_sync", UV_SYNC),
            ("uv_run_noop", UV_RUN_NOOP),
        ],
    )

    warm_state()
    results["scenarios"]["python_touched"] = measure_scenario(
        "python touched",
        before=lambda: touch(PYTHON_FILE),
        cmds=[
            ("uv_sync", UV_SYNC),
            ("uv_run_noop", UV_RUN_NOOP),
        ],
    )

    warm_state()
    results["scenarios"]["both_touched"] = measure_scenario(
        "rust + python touched",
        before=lambda: (touch(RUST_FILE), touch(PYTHON_FILE)),
        cmds=[
            ("uv_sync", UV_SYNC),
            ("uv_run_noop", UV_RUN_NOOP),
        ],
    )

    # Reinstall scenarios: separated by source state.
    # First force a rebuild to make sure the staged binary matches CURRENT sources
    # (so mtime-skip will fire on the clean reinstall).
    print("\n[setup] reset cargo cache for reinstall scenarios", flush=True)
    force_rebuild()

    results["scenarios"]["forced_reinstall_clean"] = measure_scenario(
        "forced reinstall — no source changes (mtime-skip fast path)",
        before=lambda: None,
        cmds=[
            ("uv_sync_reinstall", UV_REINSTALL),
        ],
    )

    results["scenarios"]["forced_reinstall_dirty"] = measure_scenario(
        "forced reinstall — after touching .rs (cargo incremental)",
        before=lambda: touch(RUST_FILE),
        cmds=[
            ("uv_sync_reinstall", UV_REINSTALL),
        ],
    )

    out_path.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    print(f"\nWrote {out_path}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

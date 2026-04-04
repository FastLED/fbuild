#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Build and publish fbuild to PyPI.

Zero-argument release pipeline:
  1. Pre-check: fail fast if version already exists on PyPI
  2. Trigger GitHub Actions to build native binaries for all platforms
  3. Wait for builds to complete, download artifacts
  4. Assemble platform-specific wheels (native binaries, no Python runtime)
  5. Upload to PyPI

Usage:
    ./publish              # full pipeline
    ./publish --dry-run    # everything except upload
"""

from __future__ import annotations

import argparse
import base64
import csv
import hashlib
import io
import json
import re
import shutil
import stat
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST_DIR = ROOT / "dist"
WHEEL_DIR = DIST_DIR / "wheels"
WORKFLOW_FILE = "build.yml"
PYTHON_SHIMS_DIR = ROOT / "python"

# GitHub artifact name -> dist/ subdir
ARTIFACT_MAP: dict[str, str] = {
    "binaries-x86_64-unknown-linux-musl": "linux-x86_64",
    "binaries-aarch64-unknown-linux-musl": "linux-aarch64",
    "binaries-x86_64-apple-darwin": "macos-x86_64",
    "binaries-aarch64-apple-darwin": "macos-aarch64",
    "binaries-x86_64-pc-windows-msvc": "windows-x86_64",
}

# dist/ subdir -> wheel platform tags
PLATFORMS: dict[str, list[str]] = {
    "linux-x86_64": ["manylinux_2_17_x86_64", "manylinux2014_x86_64"],
    "linux-aarch64": ["manylinux_2_17_aarch64", "manylinux2014_aarch64"],
    "macos-x86_64": ["macosx_10_12_x86_64"],
    "macos-aarch64": ["macosx_11_0_arm64"],
    "windows-x86_64": ["win_amd64"],
}

# Extension filenames produced by build.yml
EXTENSION_NAMES = {"_native.abi3.so", "_native.pyd"}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess:
    log(f"  $ {' '.join(cmd)}")
    return subprocess.run(cmd, check=True, **kwargs)


def run_capture(cmd: list[str]) -> str:
    result = run(cmd, capture_output=True, text=True)
    return result.stdout.strip()


def read_project_meta() -> tuple[str, str, str, str]:
    """Return (name, version, summary, requires_python) from pyproject.toml."""
    with open(ROOT / "pyproject.toml", "rb") as f:
        data = tomllib.load(f)
    proj = data["project"]
    return (
        proj["name"],
        proj["version"],
        proj.get("description", ""),
        proj.get("requires-python", ">=3.9"),
    )


def detect_repo() -> str:
    """Detect owner/repo from git remote origin."""
    url = run_capture(["git", "remote", "get-url", "origin"])
    if url.startswith("git@"):
        url = url.split(":", 1)[1]
    elif "github.com" in url:
        url = url.split("github.com/", 1)[1]
    return url.removesuffix(".git")


def record_hash(data: bytes) -> str:
    digest = hashlib.sha256(data).digest()
    return "sha256=" + base64.urlsafe_b64encode(digest).rstrip(b"=").decode()


# ---------------------------------------------------------------------------
# Failed-build log retrieval
# ---------------------------------------------------------------------------

def download_failed_logs(repo: str, run_id: int) -> list[Path]:
    """Download logs for failed jobs, organized per target.

    Returns a list of log file paths that were saved.
    """
    log(f"\n==> Downloading logs for failed jobs in run {run_id}")

    logs_dir = DIST_DIR / "logs"
    logs_dir.mkdir(parents=True, exist_ok=True)

    # Identify which jobs failed
    try:
        jobs_raw = run_capture([
            "gh", "run", "view", str(run_id),
            "--repo", repo,
            "--json", "jobs",
        ])
        jobs = json.loads(jobs_raw).get("jobs", [])
    except (subprocess.CalledProcessError, json.JSONDecodeError):
        jobs = []

    failed_jobs: dict[str, str] = {}  # job name -> target triple
    for job in jobs:
        if job.get("conclusion") == "failure":
            name = job.get("name", "")
            # Extract target triple from job name like "Build (x86_64-unknown-linux-gnu)"
            m = re.search(r"\(([^)]+)\)", name)
            target = m.group(1) if m else name
            failed_jobs[name] = target

    if not failed_jobs:
        log("  No failed jobs found in run metadata.")
        return []

    log(f"  Failed targets: {', '.join(failed_jobs.values())}")

    # Download the failed logs (tab-delimited: job\tstep\tlog_line)
    try:
        result = subprocess.run(
            ["gh", "run", "view", str(run_id), "--repo", repo, "--log-failed"],
            capture_output=True, timeout=120,
        )
        raw_output = (result.stdout or result.stderr or b"").decode("utf-8", errors="replace")
    except (subprocess.TimeoutExpired, FileNotFoundError) as e:
        log(f"  WARNING: Could not download logs: {e}")
        return []

    # Group log lines by job name
    per_job: dict[str, list[str]] = {name: [] for name in failed_jobs}
    for line in raw_output.splitlines():
        parts = line.split("\t", 2)
        if len(parts) >= 2:
            job_name = parts[0]
            log_line = parts[2] if len(parts) == 3 else parts[1]
            if job_name in per_job:
                per_job[job_name].append(log_line)
            else:
                # Fuzzy match — gh sometimes abbreviates job names
                for known in per_job:
                    if known.startswith(job_name) or job_name.startswith(known):
                        per_job[known].append(log_line)
                        break

    # Save per-target log files and display
    saved: list[Path] = []
    preview_lines = 30
    for job_name, target in failed_jobs.items():
        lines = per_job.get(job_name, [])
        log_file = logs_dir / f"failed-{target}-{run_id}.log"
        log_file.write_text("\n".join(lines) + "\n" if lines else "(no log output)\n", encoding="utf-8")
        saved.append(log_file)

        log(f"\n  --- {target} ({len(lines)} lines) ---")
        log(f"  Log: {log_file}")
        if len(lines) > preview_lines:
            log(f"  ... (showing last {preview_lines} of {len(lines)} lines)")
            for l in lines[-preview_lines:]:
                log(f"  | {l}")
        else:
            for l in lines:
                log(f"  | {l}")

    return saved


# ---------------------------------------------------------------------------
# Step 1: PyPI version pre-check
# ---------------------------------------------------------------------------

def check_pypi_version(name: str, version: str) -> None:
    """Fail fast if this version already exists on PyPI."""
    log(f"\n=== Step 1: Pre-check PyPI for {name} {version} ===")
    url = f"https://pypi.org/pypi/{name}/json"
    try:
        with urllib.request.urlopen(url, timeout=10) as resp:
            data = json.loads(resp.read())
        existing = set(data.get("releases", {}).keys())
        if version in existing:
            log(f"  ERROR: {name} {version} already exists on PyPI.")
            log(f"  Bump the version in pyproject.toml before publishing.")
            sys.exit(1)
        log(f"  {name} {version} is available (existing: {', '.join(sorted(existing)) or 'none'})")
    except urllib.error.HTTPError as e:
        if e.code == 404:
            log(f"  {name} not yet on PyPI (first publish)")
        else:
            log(f"  WARNING: PyPI check failed (HTTP {e.code}), continuing anyway")
    except (urllib.error.URLError, TimeoutError):
        log(f"  WARNING: Could not reach PyPI, continuing anyway")


# ---------------------------------------------------------------------------
# Step 2: Trigger GitHub Actions build
# ---------------------------------------------------------------------------

def trigger_and_wait(repo: str) -> int:
    """Trigger build workflow on HEAD, wait for completion, return run ID."""
    log(f"\n=== Step 2: Build native binaries ({repo}) ===")

    head_sha = run_capture(["git", "rev-parse", "HEAD"])
    branch = run_capture(["git", "rev-parse", "--abbrev-ref", "HEAD"])
    log(f"  Branch: {branch} ({head_sha[:12]})")

    # Snapshot existing runs to detect the new one
    existing_raw = run_capture([
        "gh", "run", "list",
        "--repo", repo,
        "--workflow", WORKFLOW_FILE,
        "--limit", "1",
        "--json", "databaseId",
    ])
    existing_ids = {r["databaseId"] for r in json.loads(existing_raw)} if existing_raw else set()

    # Trigger — workflow lives on default branch; pass current branch as input
    log(f"  Triggering {WORKFLOW_FILE} for ref={branch}...")
    run(["gh", "workflow", "run", WORKFLOW_FILE, "--repo", repo, "--field", f"ref={branch}"])

    # Wait for run to appear
    log("  Waiting for run to start...")
    run_id = None
    for _ in range(30):
        time.sleep(2)
        result = run_capture([
            "gh", "run", "list",
            "--repo", repo,
            "--workflow", WORKFLOW_FILE,
            "--limit", "5",
            "--json", "databaseId,status",
        ])
        for r in json.loads(result):
            if r["databaseId"] not in existing_ids:
                run_id = r["databaseId"]
                break
        if run_id:
            break

    if not run_id:
        log("  ERROR: Timed out waiting for workflow run to appear.")
        sys.exit(1)

    log(f"  Run {run_id} started")
    log(f"  https://github.com/{repo}/actions/runs/{run_id}")

    # Wait for completion (30 min timeout, 15 min queued timeout)
    timeout = 1800
    queued_timeout = 900
    start = time.time()
    queued_since: float | None = None
    while time.time() - start < timeout:
        result = run_capture([
            "gh", "run", "view", str(run_id),
            "--repo", repo,
            "--json", "status,conclusion",
        ])
        data = json.loads(result)
        status = data["status"]

        if status == "completed":
            if data.get("conclusion") == "success":
                elapsed = int(time.time() - start)
                log(f"  Build completed in {elapsed}s")
                return run_id
            log(f"  ERROR: Build failed: {data.get('conclusion')}")
            log(f"  https://github.com/{repo}/actions/runs/{run_id}")
            download_failed_logs(repo, run_id)
            sys.exit(1)

        # Detect stuck-in-queue (no runners available)
        if status in ("queued", "waiting", "pending"):
            if queued_since is None:
                queued_since = time.time()
            elif time.time() - queued_since > queued_timeout:
                log(f"  ERROR: Run has been queued for >{queued_timeout}s with no runner picking it up.")
                log(f"  This usually means no GitHub Actions runners are available for the workflow.")
                log(f"  Check: https://github.com/{repo}/actions/runs/{run_id}")
                sys.exit(1)
        else:
            queued_since = None  # Reset if status progresses (e.g. "in_progress")

        elapsed = int(time.time() - start)
        log(f"  [{elapsed}s] {status}...")
        time.sleep(15)

    log(f"  ERROR: Build timed out after {timeout}s")
    sys.exit(1)


# ---------------------------------------------------------------------------
# Step 3: Download artifacts
# ---------------------------------------------------------------------------

def download_artifacts(repo: str, run_id: int) -> None:
    """Download build artifacts and organize into dist/."""
    log(f"\n=== Step 3: Download artifacts from run {run_id} ===")

    if DIST_DIR.exists():
        shutil.rmtree(DIST_DIR)
    DIST_DIR.mkdir()

    tmp = DIST_DIR / "_tmp"
    tmp.mkdir()
    run(["gh", "run", "download", str(run_id), "--repo", repo, "--dir", str(tmp)])

    found = 0
    for artifact_name, subdir in ARTIFACT_MAP.items():
        src = tmp / artifact_name
        if not src.exists():
            log(f"  WARNING: Missing {artifact_name}")
            continue

        dest = DIST_DIR / subdir
        dest.mkdir(parents=True, exist_ok=True)

        for f in src.iterdir():
            target = dest / f.name
            shutil.copy2(f, target)
            if not f.name.endswith(".exe"):
                target.chmod(0o755)
            size_mb = target.stat().st_size / (1024 * 1024)
            log(f"  {subdir}/{f.name} ({size_mb:.1f} MB)")

        found += 1

    shutil.rmtree(tmp)
    log(f"  {found}/{len(ARTIFACT_MAP)} platforms downloaded")

    if found == 0:
        log("  ERROR: No artifacts downloaded.")
        sys.exit(1)


# ---------------------------------------------------------------------------
# Step 4: Build wheels
# ---------------------------------------------------------------------------

def build_wheel(
    name: str,
    version: str,
    summary: str,
    requires_python: str,
    platform_subdir: str,
    plat_tags: list[str],
) -> Path | None:
    bin_dir = DIST_DIR / platform_subdir
    if not bin_dir.exists():
        return None

    # Separate CLI binaries from PyO3 extension
    cli_binaries: list[Path] = []
    extension_file: Path | None = None
    for f in sorted(bin_dir.iterdir()):
        if f.name in EXTENSION_NAMES:
            extension_file = f
        else:
            cli_binaries.append(f)

    if not cli_binaries:
        return None

    has_extension = extension_file is not None
    name_norm = name.replace("-", "_")
    tag_plat = ".".join(plat_tags)
    data_dir = f"{name_norm}-{version}.data"
    dist_info = f"{name_norm}-{version}.dist-info"

    # abi3 tag when extension is present, generic py3 otherwise
    if has_extension:
        tag_prefix = "cp39-abi3"
        wheel_filename = f"{name_norm}-{version}-cp39-abi3-{tag_plat}.whl"
    else:
        tag_prefix = "py3-none"
        wheel_filename = f"{name_norm}-{version}-py3-none-{tag_plat}.whl"

    metadata = (
        f"Metadata-Version: 2.1\n"
        f"Name: {name}\n"
        f"Version: {version}\n"
        f"Summary: {summary}\n"
        f"Requires-Python: {requires_python}\n"
    )

    wheel_meta = (
        f"Wheel-Version: 1.0\n"
        f"Generator: fbuild-publish\n"
        f"Root-Is-Purelib: false\n"
    )
    for pt in plat_tags:
        wheel_meta += f"Tag: {tag_prefix}-{pt}\n"

    exec_perms = (
        stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR
        | stat.S_IRGRP | stat.S_IXGRP
        | stat.S_IROTH | stat.S_IXOTH
    )

    WHEEL_DIR.mkdir(parents=True, exist_ok=True)
    wheel_path = WHEEL_DIR / wheel_filename
    record_rows: list[tuple[str, str, int]] = []

    def add_file(whl: zipfile.ZipFile, arcname: str, data: bytes, executable: bool = False) -> None:
        info = zipfile.ZipInfo(arcname)
        info.compress_type = zipfile.ZIP_DEFLATED
        if executable:
            info.external_attr = exec_perms << 16
        whl.writestr(info, data)
        record_rows.append((arcname, record_hash(data), len(data)))

    with zipfile.ZipFile(wheel_path, "w", zipfile.ZIP_DEFLATED) as whl:
        # CLI binaries → .data/scripts/
        for binary in cli_binaries:
            add_file(whl, f"{data_dir}/scripts/{binary.name}", binary.read_bytes(), executable=True)

        # Python shims + extension → fbuild/ package
        if has_extension:
            # Add Python shim files from python/fbuild/
            for shim in sorted(PYTHON_SHIMS_DIR.rglob("*.py")):
                rel = shim.relative_to(PYTHON_SHIMS_DIR)
                add_file(whl, str(rel).replace("\\", "/"), shim.read_bytes())

            # Add compiled extension into fbuild/ package
            add_file(
                whl,
                f"{name_norm}/{extension_file.name}",
                extension_file.read_bytes(),
                executable=True,
            )

        # dist-info
        meta_bytes = metadata.encode()
        add_file(whl, f"{dist_info}/METADATA", meta_bytes)

        wheel_bytes = wheel_meta.encode()
        add_file(whl, f"{dist_info}/WHEEL", wheel_bytes)

        buf = io.StringIO()
        writer = csv.writer(buf, lineterminator="\n")
        for row in record_rows:
            writer.writerow(row)
        writer.writerow((f"{dist_info}/RECORD", "", ""))
        whl.writestr(f"{dist_info}/RECORD", buf.getvalue().encode())

    size_mb = wheel_path.stat().st_size / (1024 * 1024)
    ext_label = " +ext" if has_extension else " (cli-only)"
    log(f"  {wheel_filename} ({size_mb:.1f} MB){ext_label}")
    return wheel_path


def build_all_wheels(name: str, version: str, summary: str, requires_python: str) -> list[Path]:
    log(f"\n=== Step 4: Build wheels ({name} {version}) ===")

    if WHEEL_DIR.exists():
        shutil.rmtree(WHEEL_DIR)

    wheels: list[Path] = []
    for subdir, plat_tags in PLATFORMS.items():
        whl = build_wheel(name, version, summary, requires_python, subdir, plat_tags)
        if whl:
            wheels.append(whl)

    if not wheels:
        log("  ERROR: No wheels were built.")
        sys.exit(1)

    log(f"  {len(wheels)} wheel(s) ready")
    return wheels


# ---------------------------------------------------------------------------
# Step 5: Upload
# ---------------------------------------------------------------------------

def upload_wheels(wheels: list[Path], name: str, version: str) -> None:
    log(f"\n=== Step 5: Upload to PyPI ===")
    upload_cmd = ["uv", "publish"]
    upload_cmd.extend(str(w) for w in sorted(wheels))
    run(upload_cmd)
    log(f"\n  Published: https://pypi.org/project/{name}/{version}/")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(description="Build and publish fbuild to PyPI")
    parser.add_argument("--dry-run", action="store_true", help="Build wheels but do not upload.")
    args = parser.parse_args()

    # Verify prerequisites
    try:
        run_capture(["gh", "--version"])
    except FileNotFoundError:
        log("ERROR: 'gh' (GitHub CLI) is not installed.")
        sys.exit(1)

    name, version, summary, requires_python = read_project_meta()
    repo = detect_repo()
    log(f"Publishing {name} {version} from {repo}")

    # Step 1: Fail fast if version exists
    check_pypi_version(name, version)

    # Step 2: Build native binaries on all platforms
    run_id = trigger_and_wait(repo)

    # Step 3: Download artifacts
    download_artifacts(repo, run_id)

    # Step 4: Build platform wheels
    wheels = build_all_wheels(name, version, summary, requires_python)

    # Step 5: Upload
    if args.dry_run:
        log(f"\n=== Step 5: Upload (skipped — dry run) ===")
        for w in wheels:
            log(f"  {w.name}")
    else:
        upload_wheels(wheels, name, version)

    log("\n=== Done ===")


if __name__ == "__main__":
    main()

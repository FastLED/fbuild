#!/usr/bin/env python3
"""Capture baseline ELF / TU-count measurements for fbuild GitHub issue #205.

The acceptance criteria in #205 quote raw thresholds (TU count <= 250,
.bss <= 3 KB, .dmabuffers <= 1 KB, total memory <= baseline + 1%) but no
baseline numbers are recorded anywhere yet. This script captures the
foundation-landed-SHA baseline so Phase 6 acceptance tests have something
concrete to anchor "+1%" or "<= 250" claims to.

For each target it:

1. Builds the fixture project via ``uv run soldr cargo run -p fbuild-cli --
   build <project> -e <env>`` plus a separate ``-t compiledb`` invocation to
   produce ``compile_commands.json``.
2. Counts distinct ``file`` entries in the resulting compile_commands.json.
3. Probes the resulting firmware.elf for ``.text`` / ``.data`` / ``.bss`` /
   ``.dmabuffers`` section sizes via ``arm-none-eabi-size`` (preferred for
   ARM targets) or ``llvm-size``.
4. Scans compile_commands.json for compiled FNET / Snooze / RadioHead /
   mbedtls source files (the libraries that #204 root-caused as
   wrongly-selected). Counts are by ``file`` field only — never by the
   ``-I.../libraries/<lib>`` header search-path flag, which is on every
   TU regardless of which sources were compiled.

Skipping behaviour:

* Missing project path -> ``skip`` row in the status table; continue.
* Build failure -> ``build failed`` row; capture stderr tail; continue.
* Missing size tool -> section sizes recorded as ``unavailable``; TU count
  and library scan still captured.

Usage::

    uv run python ci/measure_baseline_205.py
    uv run python ci/measure_baseline_205.py --out tasks/baseline-205.md
    uv run python ci/measure_baseline_205.py --targets teensyLC teensy41

Exit code is 0 if at least one target produced data, 1 if every target
was skipped or failed.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import List, Optional

# ── Repo / target registry ───────────────────────────────────────────────────
REPO_ROOT = Path(__file__).resolve().parent.parent

# (env_name, fbuild_project_path, sketch_label)
# The sketch label is descriptive only — fbuild builds whatever ``src/`` ships
# in the fixture project. teensyLC, teensy30, teensy41, stm32f103c8 all ship a
# Blink-class sketch as ``src/main.ino``.
TARGETS = [
    # WHY: env names must match the [env:...] keys in each fixture's
    # platformio.ini exactly (case-sensitive). The teensy LC env is
    # `teensylc` lowercase per tests/platform/teensylc/platformio.ini.
    ("teensylc", "tests/platform/teensylc", "Blink"),
    ("teensy30", "tests/platform/teensy30", "Blink"),
    ("teensy41", "tests/platform/teensy41", "Blink"),
    ("stm32f103c8", "tests/platform/stm32f103c8", "Blink"),
]

# Libraries that #204 root-caused as wrongly selected on Blink builds.
EXCLUDED_LIB_NEEDLES = ["FNET", "Snooze", "RadioHead", "mbedtls"]

# Sections we report on by default. ``.dmabuffers`` is teensy-specific.
CORE_SECTIONS = [".text", ".data", ".bss"]
EXTRA_SECTIONS_TEENSY = [".dmabuffers"]


def _safe_repo_relpath(p: Path) -> str:
    """Repo-relative POSIX path that never raises.

    ``Path.relative_to`` raises ``ValueError`` when the resolved path is not
    strictly under ``REPO_ROOT`` (e.g. symlink trees, ``~`` expansion under
    CI). ``os.path.relpath`` always returns a string, so a single odd path
    won't void the entire baseline run after a successful build.
    """
    rel = os.path.relpath(p, REPO_ROOT)
    return Path(rel).as_posix()


# ── Result types ─────────────────────────────────────────────────────────────
@dataclass
class TargetResult:
    env: str
    project: Path
    sketch: str
    status: str = "pending"  # ok | skip | build_failed
    tu_count: Optional[int] = None
    size_tool: Optional[str] = None
    sections: dict = field(default_factory=dict)  # section name -> int bytes (or None)
    excluded_lib_hits: dict = field(default_factory=dict)  # needle -> int hit count
    notes: str = ""
    elf_path: Optional[Path] = None
    compdb_path: Optional[Path] = None


# ── Tool discovery ───────────────────────────────────────────────────────────
def _platformio_size_candidates() -> List[str]:
    """Return likely paths to size binaries inside ~/.platformio/packages."""
    home = Path(os.path.expanduser("~"))
    pio = home / ".platformio" / "packages"
    if not pio.is_dir():
        return []
    suffix = ".exe" if os.name == "nt" else ""
    out: List[str] = []
    for pkg_dir in pio.iterdir():
        bin_dir = pkg_dir / "bin"
        if not bin_dir.is_dir():
            continue
        candidate = bin_dir / f"arm-none-eabi-size{suffix}"
        if candidate.is_file():
            out.append(str(candidate))
    return out


def find_size_tool(prefer_arm: bool) -> Optional[str]:
    """Find a ``size`` binary on PATH (or in a known PlatformIO toolchain).

    If ``prefer_arm`` is True, ``arm-none-eabi-size`` is searched first.
    """
    arm = shutil.which("arm-none-eabi-size")
    llvm = shutil.which("llvm-size")
    plain = shutil.which("size")
    pio = _platformio_size_candidates()

    if prefer_arm:
        order = [arm] + pio + [llvm, plain]
    else:
        order = [llvm, arm, plain] + pio
    for cand in order:
        if cand:
            return cand
    return None


# ── Build invocation ─────────────────────────────────────────────────────────
def _run(cmd: List[str], cwd: Optional[Path] = None, timeout: int = 1800) -> subprocess.CompletedProcess:
    """Run a command capturing both stdout and stderr.

    Returns a CompletedProcess; never raises CalledProcessError.
    """
    return subprocess.run(
        cmd,
        cwd=str(cwd) if cwd is not None else None,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def build_target(project: Path, env: str) -> tuple[bool, str]:
    """Run ``fbuild build`` for the given project/env. Returns (ok, log_tail)."""
    cmd = [
        "uv",
        "run",
        "soldr",
        "cargo",
        "run",
        "--quiet",
        "-p",
        "fbuild-cli",
        "--",
        "build",
        str(project),
        "-e",
        env,
    ]
    result = _run(cmd, cwd=REPO_ROOT, timeout=1800)
    log_tail = (result.stdout or "") + (result.stderr or "")
    log_tail = log_tail.strip().splitlines()[-25:]
    return result.returncode == 0, "\n".join(log_tail)


def generate_compdb(project: Path, env: str) -> tuple[bool, str]:
    """Generate compile_commands.json for the project/env."""
    cmd = [
        "uv",
        "run",
        "soldr",
        "cargo",
        "run",
        "--quiet",
        "-p",
        "fbuild-cli",
        "--",
        "build",
        str(project),
        "-e",
        env,
        "-t",
        "compiledb",
    ]
    result = _run(cmd, cwd=REPO_ROOT, timeout=1800)
    log_tail = (result.stdout or "") + (result.stderr or "")
    log_tail = log_tail.strip().splitlines()[-25:]
    return result.returncode == 0, "\n".join(log_tail)


# ── Parsers ──────────────────────────────────────────────────────────────────
def parse_compile_commands(path: Path) -> tuple[Optional[int], dict]:
    """Return (tu_count, excluded_lib_hits).

    ``excluded_lib_hits[lib]`` counts TUs whose ``file`` field is under
    ``.../libraries/<lib>/`` — i.e. the library actually had a source
    file compiled. We deliberately do NOT scan the ``arguments`` /
    ``command`` fields: the framework's full ``-I.../libraries/<lib>``
    flag is propagated to every TU as a header search path, so a naive
    substring match would report counts equal to the TU count even
    when zero ``<lib>/*.c`` files were compiled.

    AC#1 from FastLED/fbuild#205 is "FNET / Snooze / RadioHead /
    mbedtls are not compiled" — that question is answered by the
    ``file`` field alone.
    """
    try:
        with path.open(encoding="utf-8") as fh:
            entries = json.load(fh)
    except (OSError, json.JSONDecodeError):  # pragma: no cover - defensive
        return None, {needle: 0 for needle in EXCLUDED_LIB_NEEDLES}

    files = {entry.get("file") for entry in entries if isinstance(entry, dict)}
    files.discard(None)
    tu_count = len(files)

    hits = {needle: 0 for needle in EXCLUDED_LIB_NEEDLES}
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        file_field = entry.get("file")
        if not isinstance(file_field, str):
            continue
        # Normalize separators so the same check works on Windows + Unix.
        normalized = file_field.replace("\\", "/")
        for needle in EXCLUDED_LIB_NEEDLES:
            if f"/libraries/{needle}/" in normalized:
                hits[needle] += 1
    return tu_count, hits


def parse_size_output(stdout: str, sections: List[str]) -> dict:
    """Parse ``arm-none-eabi-size -A`` (Berkeley-format fallback) output.

    ``-A`` (sysv) output is one section per line:
        section            size      addr
        .text             12344         0
    Berkeley format groups sections, but ``-A`` is universally supported.
    """
    out: dict = {section: None for section in sections}
    for line in stdout.splitlines():
        parts = line.split()
        if len(parts) < 2:
            continue
        name = parts[0]
        if name in out:
            try:
                out[name] = int(parts[1])
            except ValueError:
                continue
    return out


def measure_sections(elf_path: Path, size_tool: str, want_sections: List[str]) -> dict:
    cmd = [size_tool, "-A", str(elf_path)]
    result = _run(cmd, timeout=60)
    if result.returncode != 0:
        return {section: None for section in want_sections}
    return parse_size_output(result.stdout, want_sections)


# ── Discovery helpers ────────────────────────────────────────────────────────
def find_artifacts(project: Path, env: str) -> tuple[Optional[Path], Optional[Path]]:
    """Locate firmware.elf and compile_commands.json after a build."""
    fbuild_dir = project / ".fbuild"
    build_root = fbuild_dir / "build" / env

    elf_path: Optional[Path] = None
    if build_root.is_dir():
        # Try profile subdirs first, then base.
        for candidate_dir in [build_root / "release", build_root / "quick", build_root]:
            cand = candidate_dir / "firmware.elf"
            if cand.is_file():
                elf_path = cand
                break
        if elf_path is None:
            # Fallback: scan recursively for firmware.elf (any depth).
            for found in build_root.rglob("firmware.elf"):
                elf_path = found
                break

    compdb_path = project / "compile_commands.json"
    if not compdb_path.is_file():
        compdb_path = None

    return elf_path, compdb_path


# ── Markdown rendering ───────────────────────────────────────────────────────
def render_markdown(results: List[TargetResult], git_sha: str, branch: str, cargo_version: str) -> str:
    iso = _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    size_tool_used = next((r.size_tool for r in results if r.size_tool), "unavailable")
    lines: List[str] = []
    lines.append("# Baseline measurements for #205")
    lines.append("")
    lines.append(f"Captured: {iso}")
    lines.append(f"Git SHA: {git_sha}")
    lines.append(f"Branch: {branch}")
    lines.append(f"Tooling: {cargo_version}, size tool: {size_tool_used}")
    lines.append("")
    lines.append(
        "Generated by `uv run python ci/measure_baseline_205.py`. "
        "See module docstring for methodology."
    )
    lines.append("")

    for r in results:
        lines.append(f"## {r.env} / {r.sketch}")
        lines.append("")
        if r.status == "skip":
            lines.append(f"_Skipped: {r.notes}_")
            lines.append("")
            continue
        if r.status == "build_failed":
            lines.append(f"_Build failed: {r.notes}_")
            lines.append("")
            if r.tu_count is not None:
                lines.append(f"- TU count (compile_commands.json): {r.tu_count}")
            lines.append("")
            continue

        lines.append(f"- Project: `{_safe_repo_relpath(r.project)}`")
        if r.elf_path is not None:
            lines.append(f"- ELF: `{_safe_repo_relpath(r.elf_path)}`")
        lines.append(
            f"- TU count: {r.tu_count if r.tu_count is not None else 'unavailable'}"
        )
        for section, value in r.sections.items():
            if value is None:
                lines.append(f"- {section}: section absent or size tool unavailable")
            else:
                lines.append(f"- {section}: {value:,} bytes")
        lines.append(
            "- Excluded-library source files compiled (AC#1 must be 0 for all):"
        )
        for needle in EXCLUDED_LIB_NEEDLES:
            count = r.excluded_lib_hits.get(needle, 0)
            label = "0 (not compiled)" if count == 0 else f"{count} TU(s) compiled"
            lines.append(f"  - {needle}: {label}")
        if r.notes:
            lines.append(f"- Notes: {r.notes}")
        lines.append("")

    # Status summary table.
    lines.append("## Build status")
    lines.append("")
    lines.append("| env | build | TU count | size tool | notes |")
    lines.append("|---|---|---|---|---|")
    for r in results:
        tu = "-" if r.tu_count is None else str(r.tu_count)
        tool = r.size_tool or "-"
        if r.status == "ok":
            build = "ok"
        elif r.status == "skip":
            build = "skip"
        elif r.status == "build_failed":
            build = "build failed"
        else:
            build = r.status
        notes = (r.notes or "").replace("|", "\\|").replace("\n", " ")
        if len(notes) > 90:
            notes = notes[:87] + "..."
        lines.append(f"| {r.env} | {build} | {tu} | {tool} | {notes} |")
    lines.append("")

    lines.append("## Run command")
    lines.append("")
    lines.append("```")
    lines.append("uv run python ci/measure_baseline_205.py --out tasks/baseline-205.md")
    lines.append("```")
    lines.append("")

    return "\n".join(lines)


# ── Main ─────────────────────────────────────────────────────────────────────
def measure_one(env: str, project_rel: str, sketch: str) -> TargetResult:
    project = (REPO_ROOT / project_rel).resolve()
    result = TargetResult(env=env, project=project, sketch=sketch)

    if not project.is_dir():
        result.status = "skip"
        result.notes = f"project path missing: {project_rel}"
        print(f"[skip] {env}: {result.notes}", file=sys.stderr)
        return result

    is_teensy = env.lower().startswith("teensy")
    want_sections = list(CORE_SECTIONS)
    if is_teensy:
        want_sections.extend(EXTRA_SECTIONS_TEENSY)

    print(f"[build] {env}: building {project_rel} ...", file=sys.stderr)
    ok, log = build_target(project, env)
    if not ok:
        result.status = "build_failed"
        result.notes = f"build failed: {log[-300:] if log else 'no output'}"
        print(f"[fail] {env}: build failed", file=sys.stderr)
        return result

    print(f"[compdb] {env}: generating compile_commands.json ...", file=sys.stderr)
    compdb_ok, compdb_log = generate_compdb(project, env)
    if not compdb_ok:
        # Don't abort — try to find an elf anyway and record what we have.
        result.notes = (result.notes + f" compiledb generation failed: {compdb_log[-200:]}").strip()

    elf_path, compdb_path = find_artifacts(project, env)
    result.elf_path = elf_path
    result.compdb_path = compdb_path

    if compdb_path is not None:
        tu, hits = parse_compile_commands(compdb_path)
        result.tu_count = tu
        result.excluded_lib_hits = hits
    else:
        result.excluded_lib_hits = {needle: 0 for needle in EXCLUDED_LIB_NEEDLES}
        note = "compile_commands.json not found"
        result.notes = (result.notes + " " + note).strip()

    if elf_path is None:
        result.status = "build_failed"
        note = "firmware.elf not found after build"
        result.notes = (result.notes + " " + note).strip()
        print(f"[fail] {env}: {note}", file=sys.stderr)
        return result

    size_tool = find_size_tool(prefer_arm=True)
    if size_tool is None:
        result.sections = {section: None for section in want_sections}
        note = "no size tool found (tried arm-none-eabi-size, llvm-size, size)"
        result.notes = (result.notes + " " + note).strip()
    else:
        result.size_tool = Path(size_tool).name
        result.sections = measure_sections(elf_path, size_tool, want_sections)

    result.status = "ok"
    print(
        f"[ok]   {env}: TU={result.tu_count} sections={result.sections}",
        file=sys.stderr,
    )
    return result


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description=(__doc__ or "").split("\n")[0],
    )
    parser.add_argument(
        "--out",
        default="tasks/baseline-205.md",
        help="Path (relative to repo root) for the markdown output.",
    )
    parser.add_argument(
        "--targets",
        nargs="*",
        default=None,
        help="Subset of target env names to measure (default: all).",
    )
    args = parser.parse_args(argv)

    targets = TARGETS
    if args.targets:
        wanted = set(args.targets)
        targets = [t for t in TARGETS if t[0] in wanted]
        missing = wanted - {t[0] for t in TARGETS}
        if missing:
            print(f"[warn] unknown targets ignored: {sorted(missing)}", file=sys.stderr)

    results: List[TargetResult] = []
    for env, rel, sketch in targets:
        try:
            results.append(measure_one(env, rel, sketch))
        except Exception as exc:  # pragma: no cover - defensive
            r = TargetResult(env=env, project=(REPO_ROOT / rel).resolve(), sketch=sketch)
            r.status = "build_failed"
            r.notes = f"unhandled exception: {exc!r}"
            results.append(r)
            print(f"[fail] {env}: {exc!r}", file=sys.stderr)

    git_sha = _run(["git", "rev-parse", "HEAD"], cwd=REPO_ROOT).stdout.strip() or "unknown"
    branch = (
        _run(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=REPO_ROOT).stdout.strip()
        or "unknown"
    )
    cargo_proc = _run(["uv", "run", "soldr", "cargo", "--version"], cwd=REPO_ROOT, timeout=120)
    cargo_version = cargo_proc.stdout.strip() or "unknown"

    out_path = (REPO_ROOT / args.out).resolve()
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        render_markdown(results, git_sha, branch, cargo_version),
        encoding="utf-8",
    )

    # Console summary.
    print()
    print(f"Baseline written to: {out_path}")
    print()
    print(f"{'env':<14} {'status':<14} {'TUs':>5}  sections")
    print("-" * 78)
    for r in results:
        section_summary = ", ".join(
            f"{name}={value}" if value is not None else f"{name}=?"
            for name, value in r.sections.items()
        )
        tu = "-" if r.tu_count is None else str(r.tu_count)
        print(f"{r.env:<14} {r.status:<14} {tu:>5}  {section_summary}")

    any_ok = any(r.status == "ok" for r in results)
    has_data = any(r.tu_count is not None or r.status == "ok" for r in results)
    return 0 if (any_ok or has_data) else 1


if __name__ == "__main__":
    sys.exit(main())

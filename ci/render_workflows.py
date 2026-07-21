#!/usr/bin/env python3
"""Render `on:` triggers for per-board build-*.yml workflows from a single SOT.

Sources of truth:
  - ci/board_families.json  -- per-board metadata + family -> crate paths
  - ci/ci_common_paths.txt  -- paths that force-run every per-board build

Produces (or --check verifies):
  - .github/workflows/build-<board>.yml  (rewrites only the `on:` block)

The rewritten block is wrapped in sentinel comment lines so subsequent
re-renders are deterministic:
    # >>> RENDERED-ON-BEGIN (ci/render_workflows.py) -- do not edit by hand <<<
    on:
      ...
    # >>> RENDERED-ON-END <<<

CI invokes this script with --check to enforce that committed workflows
match the SOT. See FastLED/fbuild#835.
"""
from __future__ import annotations

import argparse
import glob
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SOT_PATH = REPO / "ci" / "board_families.json"
COMMON_PATH = REPO / "ci" / "ci_common_paths.txt"
WORKFLOWS_DIR = REPO / ".github" / "workflows"
NIGHTLY_PATH = WORKFLOWS_DIR / "nightly-platforms.yml"

BEGIN_MARKER = "# >>> RENDERED-ON-BEGIN (ci/render_workflows.py) -- do not edit by hand <<<\n"
END_MARKER = "# >>> RENDERED-ON-END <<<\n"


def load_common_paths() -> list[str]:
    out: list[str] = []
    for raw in COMMON_PATH.read_text(encoding="utf-8").splitlines():
        s = raw.strip()
        if not s or s.startswith("#"):
            continue
        out.append(s)
    return out


def load_sot() -> dict:
    return json.loads(SOT_PATH.read_text(encoding="utf-8"))


def validate_source_paths(sot: dict, common_paths: list[str]) -> None:
    """Reject stale source-of-truth paths before generating workflows."""
    paths = list(common_paths)
    for family in sot["families"].values():
        paths.extend(family["crate_paths"])

    dead: list[str] = []
    for pattern in paths:
        check_pattern = pattern[:-3] if pattern.endswith("/**") else pattern
        if not glob.glob(str(REPO / check_pattern), recursive=True):
            dead.append(pattern)
    if dead:
        details = "\n".join(f"  - {path}" for path in dead)
        raise ValueError(f"SOT contains paths that match nothing:\n{details}")


def render_paths_for_board(board: dict, families: dict, common_paths: list[str]) -> list[str]:
    family = board["family"]
    if family not in families:
        raise ValueError(f"board {board['workflow']} references unknown family {family!r}")
    family_paths = list(families[family]["crate_paths"])

    paths: list[str] = []
    paths.append(f"{board['test_dir']}/**")
    paths.extend(family_paths)
    paths.extend(common_paths)
    paths.append(f".github/workflows/{board['workflow']}")

    seen: set[str] = set()
    deduped: list[str] = []
    for p in paths:
        if p in seen:
            continue
        seen.add(p)
        deduped.append(p)
    return deduped


def render_on_block(board: dict, families: dict, common_paths: list[str]) -> str:
    paths = render_paths_for_board(board, families, common_paths)
    paths_yaml = "\n".join(f"      - '{p}'" for p in paths)
    return (
        "on:\n"
        "  workflow_dispatch: {}\n"
        "  workflow_call: {}\n"
        "  push:\n"
        "    branches: [main]\n"
        "    paths:\n"
        f"{paths_yaml}\n"
        "  pull_request:\n"
        "    branches: [main]\n"
        "    paths:\n"
        f"{paths_yaml}\n"
    )


def _find_on_and_jobs(lines: list[str]) -> tuple[int, int]:
    on_start = None
    jobs_start = None
    for i, line in enumerate(lines):
        stripped = line.rstrip("\r\n")
        if on_start is None and stripped == "on:":
            on_start = i
        if stripped == "jobs:":
            jobs_start = i
            break
    if on_start is None or jobs_start is None:
        raise ValueError("workflow is missing `on:` or `jobs:` markers")
    if jobs_start < on_start:
        raise ValueError("`jobs:` appears before `on:` -- unsupported workflow shape")
    return on_start, jobs_start


def rewrite(text: str, new_on_block: str) -> str:
    """Replace the `on:` section with the rendered block, wrapped in sentinels.

    On re-render (sentinels already present) we replace between the
    sentinels exactly. On first render we locate `on:` ... up to the line
    before `jobs:` and swap that span.
    """
    if BEGIN_MARKER in text and END_MARKER in text:
        bi = text.index(BEGIN_MARKER)
        ei = text.index(END_MARKER) + len(END_MARKER)
        return text[:bi] + BEGIN_MARKER + new_on_block + END_MARKER + text[ei:]

    lines = text.splitlines(keepends=True)
    on_start, jobs_start = _find_on_and_jobs(lines)
    before = "".join(lines[:on_start])
    after = "".join(lines[jobs_start:])
    return before + BEGIN_MARKER + new_on_block + END_MARKER + "\n" + after


def _job_id(workflow: str) -> str:
    # build-uno-r4-wifi.yml -> build-uno-r4-wifi (already GH-valid)
    return workflow[:-4] if workflow.endswith(".yml") else workflow


def render_nightly(boards: list[dict]) -> str:
    """Render .github/workflows/nightly-platforms.yml from the SOT.

    Fan-out: one `uses:` job per board. A single guard job decides
    whether the matrix runs at all -- if no commits landed in the last
    24h, every downstream job is skipped via `if:`. workflow_dispatch
    exposes a `force` boolean to bypass the guard for manual reruns.
    """
    job_blocks: list[str] = []
    for b in boards:
        jid = _job_id(b["workflow"])
        job_blocks.append(
            f"  {jid}:\n"
            f"    name: {b['workflow_name']}\n"
            f"    needs: guard\n"
            f"    if: needs.guard.outputs.should_run == 'true'\n"
            f"    uses: ./.github/workflows/{b['workflow']}\n"
        )
    jobs_yaml = "\n".join(job_blocks)
    header = (
        "# Daily safety-net sweep of every per-board build workflow.\n"
        "# See FastLED/fbuild#835.\n"
        "#\n"
        "# This file is AUTOGENERATED from ci/board_families.json.\n"
        "# Edit the SOT and re-run `uv run python ci/render_workflows.py`.\n"
        "# The CI drift gate (.github/workflows/ci-workflow-drift.yml) enforces this.\n"
        "name: Nightly Platforms\n"
        "\n"
        "on:\n"
        "  schedule:\n"
        "    # 09:00 UTC = 01:00 PST (winter) / 02:00 PDT (summer). See #835.\n"
        "    - cron: '0 9 * * *'\n"
        "  workflow_dispatch:\n"
        "    inputs:\n"
        "      force:\n"
        "        description: 'Run all platform builds even without recent commits'\n"
        "        type: boolean\n"
        "        default: false\n"
        "\n"
        "jobs:\n"
        "  guard:\n"
        "    name: Guard (skip on quiet days)\n"
        "    runs-on: ubuntu-latest\n"
        "    outputs:\n"
        "      should_run: ${{ steps.check.outputs.should_run }}\n"
        "    steps:\n"
        "      - uses: actions/checkout@v6\n"
        "        with:\n"
        "          fetch-depth: 0\n"
        "      - id: check\n"
        "        env:\n"
        "          FORCE: ${{ inputs.force }}\n"
        "        run: |\n"
        "          if [ \"$FORCE\" = \"true\" ]; then\n"
        "            echo \"force=true -- running nightly sweep regardless of commit activity\"\n"
        "            echo \"should_run=true\" >> \"$GITHUB_OUTPUT\"\n"
        "            exit 0\n"
        "          fi\n"
        "          # Scheduled runs check out the default branch's HEAD; on\n"
        "          # workflow_dispatch from a feature branch this checks that\n"
        "          # branch instead, which is the right behavior for manual runs.\n"
        "          if [ -z \"$(git log --since='24 hours ago' --oneline HEAD)\" ]; then\n"
        "            echo \"No commits in the last 24h -- skipping nightly platform sweep\"\n"
        "            echo \"should_run=false\" >> \"$GITHUB_OUTPUT\"\n"
        "          else\n"
        "            echo \"Recent commits found -- running full nightly sweep\"\n"
        "            echo \"should_run=true\" >> \"$GITHUB_OUTPUT\"\n"
        "          fi\n"
        "\n"
    )
    return header + jobs_yaml


def write_if_changed(path: Path, new_text: str, check: bool, drift: list[Path], updated: list[Path]) -> None:
    if path.exists():
        old = path.read_text(encoding="utf-8")
    else:
        old = ""
    if new_text == old:
        return
    if check:
        drift.append(path)
    else:
        path.write_text(new_text, encoding="utf-8", newline="\n")
        updated.append(path)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--check", action="store_true", help="exit 1 if committed workflows drift from the SOT")
    args = ap.parse_args()

    sot = load_sot()
    families = sot["families"]
    boards = sot["boards"]
    common_paths = load_common_paths()
    try:
        validate_source_paths(sot, common_paths)
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 1

    sot_workflows = {b["workflow"] for b in boards}
    on_disk = {p.name for p in WORKFLOWS_DIR.glob("build-*.yml")}
    missing_from_sot = sorted(on_disk - sot_workflows)
    missing_on_disk = sorted(sot_workflows - on_disk)
    if missing_from_sot or missing_on_disk:
        if missing_from_sot:
            print("SOT is missing entries for these workflows:", file=sys.stderr)
            for w in missing_from_sot:
                print(f"  - {w}", file=sys.stderr)
        if missing_on_disk:
            print("SOT references workflows that don't exist on disk:", file=sys.stderr)
            for w in missing_on_disk:
                print(f"  - {w}", file=sys.stderr)
        return 1

    drift: list[Path] = []
    updated: list[Path] = []
    for board in boards:
        path = WORKFLOWS_DIR / board["workflow"]
        old = path.read_text(encoding="utf-8")
        new_on = render_on_block(board, families, common_paths)
        new = rewrite(old, new_on)
        write_if_changed(path, new, args.check, drift, updated)

    write_if_changed(NIGHTLY_PATH, render_nightly(boards), args.check, drift, updated)

    if args.check and drift:
        print("Drift detected -- the following workflows are out of sync with the SOT:", file=sys.stderr)
        for p in drift:
            print(f"  - {p.relative_to(REPO)}", file=sys.stderr)
        print("\nRun `uv run python ci/render_workflows.py` to regenerate, then commit.", file=sys.stderr)
        return 1

    if not args.check:
        if updated:
            print(f"updated {len(updated)} workflow(s):")
            for p in updated:
                print(f"  - {p.relative_to(REPO)}")
        else:
            print("no changes (all workflows already match the SOT)")

    return 0


if __name__ == "__main__":
    sys.exit(main())

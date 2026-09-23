#!/usr/bin/env python3
"""Render `on:` triggers for per-board build-*.yml workflows from a single SOT.

Sources of truth:
  - ci/board_families.json  -- per-board metadata + family -> crate paths
  - ci/ci_common_paths.txt  -- paths that force-run the CORE per-board builds

Produces (or --check verifies):
  - .github/workflows/build-<board>.yml  (rewrites only the `on:` block)

The rewritten block spans `on:` *and* `concurrency:` and is wrapped in
sentinel comment lines so subsequent re-renders are deterministic:
    # >>> RENDERED-ON-BEGIN (ci/render_workflows.py) -- do not edit by hand <<<
    on:
      ...
    concurrency:
      ...
    # >>> RENDERED-ON-END <<<

The sentinel text still says "ON" for backwards compatibility -- renaming
it would strand the old markers in every committed workflow.

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
FULL_PATH = WORKFLOWS_DIR / "ci-full.yml"
TEST_PATH = WORKFLOWS_DIR / "ci-test.yml"
MINIMAL_PATH = WORKFLOWS_DIR / "ci-minimal.yml"

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
    # Shared-code paths run the CORE boards only — uno / esp32dev / teensy41,
    # one per toolchain family shared code can plausibly break
    # (FastLED/fbuild#1396). Previously every common-code edit ran all 80
    # per-board workflows, so a one-line change in `fbuild-core` scheduled
    # ~94 checks and the merge queue serialized behind them.
    #
    # A board's own test dir, its family's crate paths, and its own workflow
    # file still trigger it directly, so family-specific work is unaffected.
    # Non-core boards are covered by the nightly sweep.
    if board.get("core", False):
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


def render_concurrency_block(board: dict) -> str:
    """Auto-cancel superseded PR runs for one per-board workflow.

    The group key is the workflow *file name*, not `github.workflow`: several
    board workflows deliberately share a display name (build-due.yml and
    build-sam3x8e_due.yml are both "Build Arduino Due"), and keying on the
    display name would make those siblings cancel each other on one PR.

    Non-`pull_request` events fall back to `github.run_id`, which puts every
    such run in its own group. That is load-bearing, not cosmetic: with a
    shared group GitHub keeps at most ONE pending run per group, so a burst of
    pushes to main would silently DROP queued SHAs -- and main pushes are what
    populate the soldr build caches and feed the release flow. Only PR runs
    are ever superseded. See FastLED/fbuild#835 for the render pipeline.
    """
    workflow = board["workflow"]
    group = (
        f"{workflow}-"
        "${{ github.event_name == 'pull_request' && github.ref || github.run_id }}"
    )
    return (
        "\n"
        "concurrency:\n"
        f"  group: {group}\n"
        "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n"
    )


def render_on_block(board: dict, families: dict, common_paths: list[str]) -> str:
    return (
        "on:\n"
        "  workflow_dispatch: {}\n"
        "  workflow_call: {}\n"
    ) + render_concurrency_block(board)


def execution_boards(boards: list[dict]) -> list[dict]:
    """Run identical build inputs once while retaining every workflow alias."""
    unique: dict[tuple[str, str, str], dict] = {}
    for board in boards:
        key = (board["test_dir"], board["env_name"], board["firmware_ext"])
        if key not in unique:
            unique[key] = {**board, "workflow_aliases": [board["workflow"]]}
        else:
            unique[key]["workflow_aliases"].append(board["workflow"])
    return list(unique.values())


def render_ci(boards: list[dict], tier: str) -> str:
    full = tier == "full"
    minimal = tier == "minimal"
    selected = execution_boards(boards if full else [b for b in boards if b.get("fractional", False)])
    entries = "".join(
        f"          - workflow: {json.dumps(b['workflow'])}\n"
        f"            workflow_name: {json.dumps(b['workflow_name'])}\n"
        f"            test_dir: {json.dumps(b['test_dir'])}\n"
        f"            env_name: {json.dumps(b['env_name'])}\n"
        f"            firmware_ext: {json.dumps(b['firmware_ext'])}\n"
        f"            workflow_aliases: {json.dumps(b['workflow_aliases'])}\n"
        for b in selected
    )
    if not selected:
        raise ValueError("fractional CI requires at least one selected board")
    name = f"ci-{tier}"
    trigger = (
        "  workflow_call:\n"
        "    inputs:\n"
        "      candidate_sha:\n"
        "        type: string\n"
        "        required: true\n"
        + (
            "    outputs:\n"
            "      coverage:\n"
            "        description: 'All full validation jobs passed on the candidate SHA'\n"
            "        value: ${{ jobs.coverage.outputs.complete }}\n"
            if full else ""
        )
        + "  workflow_dispatch:\n"
        "    inputs:\n"
        "      candidate_sha:\n"
        "        description: 'Exact 40-character commit SHA to validate'\n"
        "        type: string\n"
        "        required: true\n"
        if not minimal else
        "  push:\n"
        "    branches: [main]\n"
        "  pull_request:\n"
        "    branches: [main]\n"
        "    types: [opened, labeled, unlabeled, synchronize, reopened]\n"
        "  workflow_dispatch: {}\n"
    )
    gate = (
        f"    if: github.event_name != 'pull_request' || contains(github.event.pull_request.labels.*.name, '{name}')\n"
        if not minimal else ""
    )
    verified_ref = "${{ needs.verify.outputs.candidate_sha }}"
    verify = (
        "  verify:\n"
        + gate
        + "    runs-on: ubuntu-latest\n"
        + "    outputs:\n"
        + "      candidate_sha: ${{ steps.sha.outputs.candidate_sha }}\n"
        + "    steps:\n"
        + "      - uses: actions/checkout@v6\n"
        + "        with:\n"
        + "          ref: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.sha || inputs.candidate_sha }}\n"
        + "          persist-credentials: false\n"
        + "      - id: sha\n"
        + "        env:\n"
        + "          CANDIDATE_SHA: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.sha || inputs.candidate_sha }}\n"
        + "          WORKFLOW_SHA: ${{ github.sha }}\n"
        + "          EVENT_NAME: ${{ github.event_name }}\n"
        + "        run: |\n"
        + "          if [[ ! \"$CANDIDATE_SHA\" =~ ^[0-9a-f]{40}$ ]]; then\n"
        + "            echo 'candidate_sha must be an exact 40-character commit SHA' >&2\n"
        + "            exit 1\n"
        + "          fi\n"
        + "          if [ \"$EVENT_NAME\" != pull_request ] && [ \"$CANDIDATE_SHA\" != \"$WORKFLOW_SHA\" ]; then\n"
        + "            echo \"candidate $CANDIDATE_SHA differs from workflow revision $WORKFLOW_SHA; dispatch from a ref at the candidate commit\" >&2\n"
        + "            exit 1\n"
        + "          fi\n"
        + "          actual=$(git rev-parse HEAD)\n"
        + "          if [ \"$actual\" != \"$CANDIDATE_SHA\" ]; then\n"
        + "            echo \"checkout SHA $actual does not match $CANDIDATE_SHA\" >&2\n"
        + "            exit 1\n"
        + "          fi\n"
        + "          echo \"candidate_sha=$actual\" >> \"$GITHUB_OUTPUT\"\n"
        if not minimal else ""
    )
    host = (
        "  windows:\n"
        + gate
        + "    needs: verify\n"
        + "    uses: ./.github/workflows/check-windows.yml\n"
        + "    with:\n"
        + f"      ref: {verified_ref}\n"
        + "  dylint:\n"
        + gate
        + "    needs: verify\n"
        + "    uses: ./.github/workflows/dylint.yml\n"
        + "    with:\n"
        + f"      ref: {verified_ref}\n"
        + "      run_full: true\n"
        + "  acceptance:\n"
        + gate
        + "    needs: verify\n"
        + "    uses: ./.github/workflows/acceptance-205.yml\n"
        + "    with:\n"
        + f"      ref: {verified_ref}\n"
        + "  bench:\n"
        + gate
        + "    needs: verify\n"
        + "    uses: ./.github/workflows/bench-205.yml\n"
        + "    with:\n"
        + f"      ref: {verified_ref}\n"
        + "  qemu:\n"
        + gate
        + "    needs: verify\n"
        + "    uses: ./.github/workflows/qemu-linux-runtime.yml\n"
        + "    with:\n"
        + f"      ref: {verified_ref}\n"
        if full else ""
    )
    policy = "".join(
        f"  {job}:\n"
        + gate
        + "    needs: verify\n"
        + f"    uses: ./.github/workflows/{workflow}\n"
        + "    with:\n"
        + f"      ref: {verified_ref}\n"
        for job, workflow in (
            ("fmt", "fmt.yml"),
            ("docs", "docs.yml"),
            ("msrv", "msrv.yml"),
            ("validate_boards", "validate-boards.yml"),
            ("crate_gate", "crate-gate.yml"),
        )
    ) if full else ""
    return (
        f"# Generated by ci/render_workflows.py from ci/board_families.json.\n"
        f"name: {name}\n\n"
        "on:\n" + trigger + "\n"
        "concurrency:\n"
        f"  group: {name}-${{{{ github.event_name == 'pull_request' && github.ref || github.run_id }}}}\n"
        "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n\n"
        "jobs:\n"
        + verify
        + "  linux:\n"
        + ("    if: github.event_name != 'pull_request' || (!contains(github.event.pull_request.labels.*.name, 'ci-test') && !contains(github.event.pull_request.labels.*.name, 'ci-full'))\n" if minimal else gate)
        + ("" if minimal else "    needs: verify\n")
        + "    uses: ./.github/workflows/check-ubuntu.yml\n"
        + "    with:\n"
        + ("      ref: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.sha || github.sha }}\n" if minimal else f"      ref: {verified_ref}\n")
        + host
        + policy
        + ("" if minimal else
           "  boards:\n"
           + gate
           + "    needs: verify\n"
           + "    name: ${{ matrix.workflow_name }}\n"
           + "    strategy:\n"
           + "      fail-fast: false\n"
           + "      matrix:\n"
           + "        include:\n"
           + entries
           + "    uses: ./.github/workflows/template_build.yml\n"
           + "    with:\n"
           + "      workflow-name: ${{ matrix.workflow_name }}\n"
           + "      test-dir: ${{ matrix.test_dir }}\n"
           + "      env-name: ${{ matrix.env_name }}\n"
           + "      firmware-ext: ${{ matrix.firmware_ext }}\n"
           + f"      checkout_ref: {verified_ref}\n")
        + ("" if minimal else
            "  coverage:\n"
            + ("    name: Full coverage\n" if full else "    name: ci-test coverage\n")
            + "    if: always()\n"
            + ("    needs: [verify, boards, linux, windows, dylint, acceptance, bench, qemu, fmt, docs, msrv, validate_boards, crate_gate]\n" if full else "    needs: [verify, boards, linux]\n")
            + "    runs-on: ubuntu-latest\n"
            + "    outputs:\n"
            + "      complete: ${{ steps.complete.outputs.complete }}\n"
            + "    steps:\n"
            + "      - id: complete\n"
            + "        env:\n"
            + "          EVENT_NAME: ${{ github.event_name }}\n"
            + f"          LABEL_PRESENT: ${{{{ contains(github.event.pull_request.labels.*.name, '{name}') }}}}\n"
            + "          VERIFY: ${{ needs.verify.result }}\n"
            + "          BOARDS: ${{ needs.boards.result }}\n"
            + "          LINUX: ${{ needs.linux.result }}\n"
            + ("          WINDOWS: ${{ needs.windows.result }}\n"
               "          DYLINT: ${{ needs.dylint.result }}\n"
               "          ACCEPTANCE: ${{ needs.acceptance.result }}\n"
               "          BENCH: ${{ needs.bench.result }}\n"
               "          QEMU: ${{ needs.qemu.result }}\n"
               "          FMT: ${{ needs.fmt.result }}\n"
               "          DOCS: ${{ needs.docs.result }}\n"
               "          MSRV: ${{ needs.msrv.result }}\n"
               "          VALIDATE_BOARDS: ${{ needs.validate_boards.result }}\n"
               "          CRATE_GATE: ${{ needs.crate_gate.result }}\n" if full else "")
            + "        run: |\n"
            + "          if [ \"$EVENT_NAME\" = pull_request ] && [ \"$LABEL_PRESENT\" != true ]; then\n"
            + "            echo 'complete=false' >> \"$GITHUB_OUTPUT\"\n"
            + "            echo 'Optional tier is not selected on this PR; coverage is incomplete' >&2\n"
            + "            exit 1\n"
            + "          fi\n"
            + ("          for result in \"$VERIFY\" \"$BOARDS\" \"$LINUX\" \"$WINDOWS\" \"$DYLINT\" \"$ACCEPTANCE\" \"$BENCH\" \"$QEMU\" \"$FMT\" \"$DOCS\" \"$MSRV\" \"$VALIDATE_BOARDS\" \"$CRATE_GATE\"; do\n" if full else "          for result in \"$VERIFY\" \"$BOARDS\" \"$LINUX\"; do\n")
            + "            if [ \"$result\" != success ]; then\n"
            + "              echo \"coverage incomplete: $result\" >&2\n"
            + "              exit 1\n"
            + "            fi\n"
            + "          done\n"
            + "          echo 'complete=true' >> \"$GITHUB_OUTPUT\"\n"
        )
        + ("  test:\n"
           "    if: github.event_name == 'pull_request' && contains(github.event.pull_request.labels.*.name, 'ci-test') && !contains(github.event.pull_request.labels.*.name, 'ci-full')\n"
           "    uses: ./.github/workflows/ci-test.yml\n"
           "    with:\n"
           "      candidate_sha: ${{ github.event.pull_request.head.sha }}\n"
           "  full:\n"
           "    if: github.event_name == 'pull_request' && contains(github.event.pull_request.labels.*.name, 'ci-full')\n"
           "    uses: ./.github/workflows/ci-full.yml\n"
           "    with:\n"
           "      candidate_sha: ${{ github.event.pull_request.head.sha }}\n"
           "  selected-coverage:\n"
           "    name: CI selected coverage\n"
           "    if: always()\n"
           "    needs: [linux, test, full]\n"
           "    runs-on: ubuntu-latest\n"
           "    steps:\n"
           "      - env:\n"
           "          LINUX: ${{ needs.linux.result }}\n"
           "          TEST_SELECTED: ${{ contains(github.event.pull_request.labels.*.name, 'ci-test') }}\n"
           "          TEST_RESULT: ${{ needs.test.result }}\n"
           "          FULL_SELECTED: ${{ contains(github.event.pull_request.labels.*.name, 'ci-full') }}\n"
           "          FULL_RESULT: ${{ needs.full.result }}\n"
           "          FULL_COVERAGE: ${{ needs.full.outputs.coverage }}\n"
           "        run: |\n"
           "          if [ \"$FULL_SELECTED\" = true ]; then\n"
           "            test \"$FULL_RESULT\" = success\n"
           "            test \"$FULL_COVERAGE\" = true\n"
           "          elif [ \"$TEST_SELECTED\" = true ]; then\n"
           "            test \"$TEST_RESULT\" = success\n"
           "          else\n"
           "            test \"$LINUX\" = success\n"
           "          fi\n"
           if minimal else "")
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

    Fan-out: ONE matrix job that calls `template_build.yml` directly, once
    per board. A single guard job decides whether the sweep runs at all --
    if no commits landed in the last 24h, the build job is skipped via
    `if:`. workflow_dispatch exposes a `force` boolean to bypass the guard
    for manual reruns.

    This deliberately does NOT do the obvious thing of emitting one
    `uses: ./.github/workflows/build-<board>.yml` job per board. GitHub caps
    a single workflow file at **20 unique reusable workflows**, counting the
    whole nested tree. Referencing all ~79 per-board workflows blew straight
    past that cap, and the failure mode gives you nothing to debug: the run
    is created, immediately reports "This run likely failed because of a
    workflow file issue", and produces **zero jobs** and no logs. The YAML is
    perfectly valid, so neither a linter nor `yaml.safe_load` flags it.

    Calling the shared template with a matrix keeps the unique-reusable count
    at 1 no matter how many boards the SOT grows to.

    Matrix keys use underscores, not hyphens, on purpose: `matrix.test-dir`
    parses as a subtraction in a GitHub expression, silently yielding an
    empty value rather than an error. The hyphenated names are reintroduced
    only in the `with:` block, where they are input keys rather than
    expressions.
    """
    matrix_entries: list[str] = []
    for b in execution_boards(boards):
        matrix_entries.append(
            f"          - workflow_name: {json.dumps(b['workflow_name'])}\n"
            f"            test_dir: {json.dumps(b['test_dir'])}\n"
            f"            env_name: {json.dumps(b['env_name'])}\n"
            f"            firmware_ext: {json.dumps(b['firmware_ext'])}\n"
            f"            workflow_aliases: {json.dumps(b['workflow_aliases'])}\n"
        )
    jobs_yaml = (
        "  build:\n"
        "    name: ${{ matrix.workflow_name }}\n"
        "    needs: guard\n"
        "    if: needs.guard.outputs.should_run == 'true'\n"
        "    strategy:\n"
        # One broken board must not cancel the other 78 -- the whole point of
        # a nightly sweep is a complete picture of what is red.
        "      fail-fast: false\n"
        "      matrix:\n"
        "        include:\n" + "".join(matrix_entries) + "    uses: ./.github/workflows/template_build.yml\n"
        "    with:\n"
        "      workflow-name: ${{ matrix.workflow_name }}\n"
        "      test-dir: ${{ matrix.test_dir }}\n"
        "      env-name: ${{ matrix.env_name }}\n"
        "      firmware-ext: ${{ matrix.firmware_ext }}\n"
    )
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
        "    # 11:00 UTC = 03:00 PST (winter) / 04:00 PDT (summer). GitHub cron\n"
        "    # has no timezone, so one of the two has to drift; 3am standard\n"
        "    # time is the one asked for (FastLED/fbuild#1396). See also #835.\n"
        "    - cron: '0 11 * * *'\n"
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
    write_if_changed(MINIMAL_PATH, render_ci(boards, "minimal"), args.check, drift, updated)
    write_if_changed(TEST_PATH, render_ci(boards, "test"), args.check, drift, updated)
    write_if_changed(FULL_PATH, render_ci(boards, "full"), args.check, drift, updated)

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

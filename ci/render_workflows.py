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
    # Board workflows run as their own runs only when nightly-platforms.yml
    # dispatches them (path-selected on push to main, all boards on the
    # nightly schedule) -- that is what keeps each README badge current.
    return (
        "on:\n"
        "  workflow_dispatch:\n"
        "    inputs:\n"
        "      fbuild-run-id:\n"
        "        description: 'Run holding a prebuilt fbuild artifact (empty: compile here)'\n"
        "        type: string\n"
        "        default: ''\n"
        "      checkout_ref:\n"
        "        description: 'Commit to build (empty: the dispatched ref)'\n"
        "        type: string\n"
        "        default: ''\n"
        "  workflow_call: {}\n"
    ) + render_concurrency_block(board)


def toolchain_cache(board: dict, families: dict) -> bool:
    """Whether a board restores/saves its ~1GB fbuild toolchain cache.

    Opt-in per family, justified by a measurement recorded in the SOT: for
    most families a cold download is faster than the cache round-trip.
    """
    return bool(families[board["family"]].get("toolchain_cache", False))


def render_board_jobs(board: dict, families: dict) -> str:
    return (
        "jobs:\n"
        "  build:\n"
        "    uses: ./.github/workflows/template_build.yml\n"
        "    with:\n"
        f"      workflow-name: {json.dumps(board['workflow_name'])}\n"
        f"      test-dir: {json.dumps(board['test_dir'])}\n"
        f"      env-name: {json.dumps(board['env_name'])}\n"
        f"      firmware-ext: {json.dumps(board['firmware_ext'])}\n"
        "      checkout_ref: ${{ inputs.checkout_ref }}\n"
        f"      fbuild-artifact: ${{{{ inputs.fbuild-run-id && '{FBUILD_BIN_ARTIFACT}' || '' }}}}\n"
        "      fbuild-run-id: ${{ inputs.fbuild-run-id }}\n"
        f"      toolchain-cache: {json.dumps(toolchain_cache(board, families))}\n"
    )


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


FBUILD_BIN_ARTIFACT = "fbuild-bin-linux-debug"


def render_fbuild_bin_job(needs: str, condition: str, ref: str) -> str:
    """One job that compiles fbuild once and uploads it for every board job.

    zackees/ci.yml policy-rust "compile once, runners only execute": before
    this, each of ~76 board jobs spent ~350s compiling the same
    board-independent `fbuild-cli` + `fbuild-daemon` debug build. The
    setup-soldr inputs mirror template_build.yml's standalone compile path so
    both share one warm `fbuild-rust-debug` cache.
    """
    return (
        "  fbuild_bin:\n"
        "    name: Build fbuild (shared by board jobs)\n"
        + condition
        + f"    needs: {needs}\n"
        "    runs-on: ubuntu-latest\n"
        "    timeout-minutes: 30\n"
        "    env:\n"
        "      CARGO_TERM_COLOR: always\n"
        "      RUSTFLAGS: \"-D warnings\"\n"
        "      SOLDR_TARGET_BLOCK_FREE_GB: \"2\"\n"
        "    steps:\n"
        "      - uses: actions/checkout@v6\n"
        "        with:\n"
        f"          ref: {ref}\n"
        "          persist-credentials: false\n"
        "      - uses: zackees/setup-soldr@dfbe9627f6cb0226716b61625b99a58949162720\n"
        "        with:\n"
        "          cache-preset: foundation\n"
        "          prebuild-deps-flags: \"\"\n"
        "          prebuild-deps: none\n"
        "          linker: platform-default\n"
        "          cache-payload-warn-bytes: 2GiB\n"
        "          cache-key-suffix: fbuild-rust-debug\n"
        "          save-cache: ${{ github.ref == 'refs/heads/main' && 'true' || 'false' }}\n"
        "      - run: |\n"
        "          sudo apt-get -o Acquire::http::Timeout=30 -o Acquire::Retries=3 update\n"
        "          sudo apt-get -o Acquire::http::Timeout=30 -o Acquire::Retries=3 install -y libudev-dev pkg-config\n"
        "      - run: soldr cargo build -p fbuild-cli -p fbuild-daemon\n"
        "      - uses: actions/upload-artifact@v7\n"
        "        with:\n"
        f"          name: {FBUILD_BIN_ARTIFACT}\n"
        "          path: |\n"
        "            target/debug/fbuild\n"
        "            target/debug/fbuild-daemon\n"
        "          if-no-files-found: error\n"
        "          retention-days: 1\n"
    )


PR_DEFAULT_TIER = (
    "github.event_name == 'pull_request' && !contains(github.event.pull_request.labels.*.name, 'ci-test') "
    "&& !contains(github.event.pull_request.labels.*.name, 'ci-full')"
)


def render_pr_boards() -> str:
    """Path-selected board builds on the default PR tier.

    Same selection as the push-to-main dispatcher (ci/select_boards.py): a PR
    touching an ESP family path builds the ESP boards, shared crates build the
    core boards, docs build nothing. The boards are called directly rather
    than dispatched so they report as checks on the PR. `ci-full` still runs
    every board.
    """
    head = "${{ github.event.pull_request.head.sha }}"
    return (
        "  board_plan:\n"
        "    needs: verify\n"
        "    name: Select boards for changed paths\n"
        f"    if: {PR_DEFAULT_TIER}\n"
        "    runs-on: ubuntu-latest\n"
        "    outputs:\n"
        "      matrix: ${{ steps.select.outputs.matrix }}\n"
        "    steps:\n"
        "      - uses: actions/checkout@v6\n"
        "        with:\n"
        f"          ref: {head}\n"
        "          fetch-depth: 0\n"
        "          persist-credentials: false\n"
        "      - uses: astral-sh/setup-uv@v3\n"
        "      - id: select\n"
        "        env:\n"
        "          BASE: ${{ github.event.pull_request.base.sha }}\n"
        "        run: |\n"
        "          base=$(git merge-base \"$BASE\" HEAD || true)\n"
        "          matrix=$(cd ci && uv run --no-project python select_boards.py --matrix --base \"$base\" --head HEAD)\n"
        "          echo \"selected: $matrix\"\n"
        "          echo \"matrix=$matrix\" >> \"$GITHUB_OUTPUT\"\n"
        + render_fbuild_bin_job(
            "board_plan",
            "    if: needs.board_plan.outputs.matrix != '[]'\n",
            head,
        )
        + "  pr_boards:\n"
        "    name: ${{ matrix.workflow_name }}\n"
        "    needs: [board_plan, fbuild_bin]\n"
        "    strategy:\n"
        "      fail-fast: false\n"
        "      matrix:\n"
        "        include: ${{ fromJSON(needs.board_plan.outputs.matrix) }}\n"
        "    uses: ./.github/workflows/template_build.yml\n"
        "    with:\n"
        "      workflow-name: ${{ matrix.workflow_name }}\n"
        "      test-dir: ${{ matrix.test_dir }}\n"
        "      env-name: ${{ matrix.env_name }}\n"
        "      firmware-ext: ${{ matrix.firmware_ext }}\n"
        f"      checkout_ref: {head}\n"
        f"      fbuild-artifact: {FBUILD_BIN_ARTIFACT}\n"
        "      toolchain-cache: ${{ matrix.toolchain_cache }}\n"
    )


# zackees/ci.yml commit that provides `ci-lint reuse-check` (stdlib-only).
CI_LINT_SHA = "b93b8c4a20ce3cd476c75a278ad01a31bde2438b"
# Job names green on both the default PR tier and main pushes (zackees/ci.yml#162).
REUSE_REQUIRED_JOBS = (
    "linux / Check (ubuntu-latest)",
    "linux / Python facade tests (ubuntu-latest)",
    "CI selected coverage",
)
REUSE_JOB_TIMEOUT = 5
# Per-step minutes: checkout, setup-uv, reuse-check. Sum stays below the job cap.
REUSE_STEP_TIMEOUTS = (1, 1, 2)


def render_reuse_decision() -> str:
    """GEN-021 verified-reuse decision on main pushes, SHADOW mode only.

    Records in the job summary whether this push's tree is identical to a PR
    head whose ci-minimal run passed every REUSE_REQUIRED_JOBS job
    (zackees/ci.yml#162). `--mode shadow` always reports reuse=false, nothing
    `needs:` this job, and the job and every step are continue-on-error, so it
    can neither skip a job nor fail the run. REUSE_STEP_TIMEOUTS sum below
    REUSE_JOB_TIMEOUT, so a stall
    ends as a (tolerated) step timeout rather than a job cancellation.
    """
    required = "".join(f"          --required-job \"{job}\"\n" for job in REUSE_REQUIRED_JOBS)
    checkout_t, uv_t, check_t = REUSE_STEP_TIMEOUTS
    return (
        "  reuse_decision:\n"
        "    needs: verify\n"
        "    name: Verified reuse decision (shadow)\n"
        "    if: github.event_name == 'push' && github.ref == 'refs/heads/main'\n"
        "    runs-on: ubuntu-latest\n"
        f"    timeout-minutes: {REUSE_JOB_TIMEOUT}\n"
        "    continue-on-error: true\n"
        "    permissions:\n"
        "      contents: read\n"
        "      actions: read\n"
        "      pull-requests: read\n"
        "    steps:\n"
        "      - uses: actions/checkout@v6\n"
        "        continue-on-error: true\n"
        f"        timeout-minutes: {checkout_t}\n"
        "        with:\n"
        "          repository: zackees/ci.yml\n"
        f"          ref: {CI_LINT_SHA}\n"
        "          path: .ci-lint\n"
        "          persist-credentials: false\n"
        "      - uses: astral-sh/setup-uv@v3\n"
        "        continue-on-error: true\n"
        f"        timeout-minutes: {uv_t}\n"
        "      - name: Reuse decision (GEN-021, shadow)\n"
        "        continue-on-error: true\n"
        f"        timeout-minutes: {check_t}\n"
        "        working-directory: .ci-lint\n"
        "        env:\n"
        "          GITHUB_TOKEN: ${{ github.token }}\n"
        "        run: >-\n"
        "          uv run --no-project python -m ci_lint reuse-check\n"
        "          --workflow ci-minimal.yml --mode shadow\n"
        + required
    )


def render_local_gate_verify() -> str:
    """Enforce PR attestations; dispatch can run the source before it is attested."""
    return """  verify:
    name: Verify local gate
    runs-on: ubuntu-latest
    timeout-minutes: 5
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@v6
        with:
          ref: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.sha || github.sha }}
          fetch-depth: 2
          persist-credentials: false
      - uses: astral-sh/setup-uv@v3
      - name: Verify source-bound local proof
        if: github.event_name == 'pull_request'
        run: >-
          uvx --from git+https://github.com/zackees/ci.yml@9e44971219cd870a2263fa694debc94f57722405
          ci-lint local-gate verify --repo .
"""


def render_ci(boards: list[dict], tier: str, families: dict) -> str:
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
        f"            toolchain_cache: {json.dumps(toolchain_cache(b, families))}\n"
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
        if not minimal else render_local_gate_verify()
    )
    host = (
        "  windows:\n"
        + gate
        + "    needs: verify\n"
        + "    uses: ./.github/workflows/check-windows.yml\n"
        + "    with:\n"
        + f"      ref: {verified_ref}\n"
        + "  macos:\n"
        + gate
        + "    needs: verify\n"
        + "    uses: ./.github/workflows/check-macos.yml\n"
        + "    with:\n"
        + f"      ref: {verified_ref}\n"
        + "  dylint:\n"
        + gate
        + "    needs: verify\n"
        + "    uses: ./.github/workflows/dylint.yml\n"
        + "    with:\n"
        + f"      ref: {verified_ref}\n"
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
        + "    needs: verify\n"
        + "    uses: ./.github/workflows/check-ubuntu.yml\n"
        + "    with:\n"
        + ("      ref: ${{ github.event_name == 'pull_request' && github.event.pull_request.head.sha || github.sha }}\n" if minimal else f"      ref: {verified_ref}\n")
        + host
        + policy
        + ("" if minimal else
           render_fbuild_bin_job("verify", gate, verified_ref)
           + "  boards:\n"
           + gate
           + "    needs: [verify, fbuild_bin]\n"
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
           + f"      checkout_ref: {verified_ref}\n"
           + f"      fbuild-artifact: {FBUILD_BIN_ARTIFACT}\n"
           + "      toolchain-cache: ${{ matrix.toolchain_cache }}\n")
        + ("" if minimal else
            "  coverage:\n"
            + ("    name: Full coverage\n" if full else "    name: ci-test coverage\n")
            + "    if: always()\n"
            + ("    needs: [verify, boards, linux, windows, macos, dylint, acceptance, bench, qemu, fmt, validate_boards, crate_gate]\n" if full else "    needs: [verify, boards, linux]\n")
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
               "          MACOS: ${{ needs.macos.result }}\n"
               "          DYLINT: ${{ needs.dylint.result }}\n"
               "          ACCEPTANCE: ${{ needs.acceptance.result }}\n"
               "          BENCH: ${{ needs.bench.result }}\n"
               "          QEMU: ${{ needs.qemu.result }}\n"
               "          FMT: ${{ needs.fmt.result }}\n"
               "          VALIDATE_BOARDS: ${{ needs.validate_boards.result }}\n"
               "          CRATE_GATE: ${{ needs.crate_gate.result }}\n" if full else "")
            + "        run: |\n"
            + "          if [ \"$EVENT_NAME\" = pull_request ] && [ \"$LABEL_PRESENT\" != true ]; then\n"
            + "            echo 'complete=false' >> \"$GITHUB_OUTPUT\"\n"
            + "            echo 'Optional tier is not selected on this PR; coverage is incomplete' >&2\n"
            + "            exit 1\n"
            + "          fi\n"
            + ("          for result in \"$VERIFY\" \"$BOARDS\" \"$LINUX\" \"$WINDOWS\" \"$MACOS\" \"$DYLINT\" \"$ACCEPTANCE\" \"$BENCH\" \"$QEMU\" \"$FMT\" \"$VALIDATE_BOARDS\" \"$CRATE_GATE\"; do\n" if full else "          for result in \"$VERIFY\" \"$BOARDS\" \"$LINUX\"; do\n")
            + "            if [ \"$result\" != success ]; then\n"
            + "              echo \"coverage incomplete: $result\" >&2\n"
            + "              exit 1\n"
            + "            fi\n"
            + "          done\n"
            + "          echo 'complete=true' >> \"$GITHUB_OUTPUT\"\n"
        )
        + (render_pr_boards() + render_reuse_decision() if minimal else "")
        + ("  test:\n"
           "    needs: verify\n"
           "    if: github.event_name == 'pull_request' && contains(github.event.pull_request.labels.*.name, 'ci-test') && !contains(github.event.pull_request.labels.*.name, 'ci-full')\n"
           "    uses: ./.github/workflows/ci-test.yml\n"
           "    with:\n"
           "      candidate_sha: ${{ github.event.pull_request.head.sha }}\n"
           "  full:\n"
           "    needs: verify\n"
           "    if: github.event_name == 'pull_request' && contains(github.event.pull_request.labels.*.name, 'ci-full')\n"
           "    uses: ./.github/workflows/ci-full.yml\n"
           "    with:\n"
           "      candidate_sha: ${{ github.event.pull_request.head.sha }}\n"
           "  selected-coverage:\n"
           "    name: CI selected coverage\n"
           "    if: always()\n"
           "    needs: [linux, test, full, pr_boards]\n"
           "    runs-on: ubuntu-latest\n"
           "    steps:\n"
           "      - env:\n"
           "          LINUX: ${{ needs.linux.result }}\n"
           "          TEST_SELECTED: ${{ contains(github.event.pull_request.labels.*.name, 'ci-test') }}\n"
           "          TEST_RESULT: ${{ needs.test.result }}\n"
           "          FULL_SELECTED: ${{ contains(github.event.pull_request.labels.*.name, 'ci-full') }}\n"
           "          FULL_RESULT: ${{ needs.full.result }}\n"
           "          FULL_COVERAGE: ${{ needs.full.outputs.coverage }}\n"
           "          PR_BOARDS: ${{ needs.pr_boards.result }}\n"
           "        run: |\n"
           "          if [ \"$FULL_SELECTED\" = true ]; then\n"
           "            test \"$FULL_RESULT\" = success\n"
           "            test \"$FULL_COVERAGE\" = true\n"
           "          elif [ \"$TEST_SELECTED\" = true ]; then\n"
           "            test \"$TEST_RESULT\" = success\n"
           "          else\n"
           "            test \"$LINUX\" = success\n"
           "            # Path-selected boards: skipped when the PR touches none.\n"
           "            case \"$PR_BOARDS\" in success|skipped) ;; *) exit 1 ;; esac\n"
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
    """Render .github/workflows/nightly-platforms.yml: the board dispatcher.

    README badges track runs of each `build-<board>.yml` *file*; a board
    built from inside another workflow (a matrix over template_build.yml)
    never moves its badge. So this workflow compiles fbuild once, then
    `gh workflow run`s each selected board workflow as its own run, passing
    this run's id so the board downloads the prebuilt binary instead of
    compiling it (GITHUB_TOKEN-created `workflow_dispatch` runs are the one
    event GitHub allows to start new runs).

    Selection (ci/select_boards.py):
      - push to main: only boards whose trigger paths the push touched
        (own test dir, family crate paths, own workflow file; shared paths
        select the `core` boards only).
      - schedule: every board, so every badge refreshes daily -- skipped
        when no commits landed in 24h (nothing could have changed).
      - workflow_dispatch: every board.

    The dispatcher does not call the ~80 board workflows as reusable
    workflows: GitHub caps a workflow file at 20 unique reusable workflows
    and fails such a run with zero jobs and no logs.
    """
    del boards  # selection reads the SOT at run time via ci/select_boards.py
    header = (
        "# Board dispatcher: path-selected on push to main, all boards nightly.\n"
        "# See FastLED/fbuild#835 and ci/select_boards.py.\n"
        "#\n"
        "# This file is AUTOGENERATED from ci/board_families.json.\n"
        "# Edit the SOT and re-run `uv run python ci/render_workflows.py`.\n"
        "# The CI drift gate (.github/workflows/ci-workflow-drift.yml) enforces this.\n"
        "name: Nightly Platforms\n"
        "\n"
        "on:\n"
        "  push:\n"
        "    branches: [main]\n"
        "  schedule:\n"
        "    # 11:00 UTC = 03:00 PST (winter) / 04:00 PDT (summer). GitHub cron\n"
        "    # has no timezone, so one of the two has to drift; 3am standard\n"
        "    # time is the one asked for (FastLED/fbuild#1396). See also #835.\n"
        "    - cron: '0 11 * * *'\n"
        "  workflow_dispatch:\n"
        "    inputs:\n"
        "      force:\n"
        "        description: 'Dispatch all platform builds even without recent commits'\n"
        "        type: boolean\n"
        "        default: false\n"
        "\n"
        "jobs:\n"
        "  plan:\n"
        "    name: Select boards\n"
        "    runs-on: ubuntu-latest\n"
        "    outputs:\n"
        "      workflows: ${{ steps.select.outputs.workflows }}\n"
        "    steps:\n"
        "      - uses: actions/checkout@v6\n"
        "        with:\n"
        "          fetch-depth: 0\n"
        "      - uses: astral-sh/setup-uv@v3\n"
        "      - id: select\n"
        "        env:\n"
        "          EVENT: ${{ github.event_name }}\n"
        "          BEFORE: ${{ github.event.before }}\n"
        "        run: |\n"
        "          if [ \"$EVENT\" = push ]; then\n"
        "            args=(--base \"$BEFORE\" --head \"$GITHUB_SHA\")\n"
        "          elif [ \"$EVENT\" = schedule ] && [ -z \"$(git log --since='24 hours ago' --oneline HEAD)\" ]; then\n"
        "            echo 'No commits in the last 24h -- nothing to dispatch'\n"
        "            echo 'workflows=[]' >> \"$GITHUB_OUTPUT\"\n"
        "            exit 0\n"
        "          else\n"
        "            args=(--all)\n"
        "          fi\n"
        "          workflows=$(cd ci && uv run --no-project python select_boards.py \"${args[@]}\")\n"
        "          echo \"selected: $workflows\"\n"
        "          echo \"workflows=$workflows\" >> \"$GITHUB_OUTPUT\"\n"
        "\n"
    )
    dispatch = (
        "  dispatch:\n"
        "    name: Dispatch board workflows\n"
        "    needs: [plan, fbuild_bin]\n"
        "    runs-on: ubuntu-latest\n"
        "    permissions:\n"
        "      actions: write\n"
        "    steps:\n"
        "      - env:\n"
        "          GH_TOKEN: ${{ github.token }}\n"
        "          GH_REPO: ${{ github.repository }}\n"
        "          WORKFLOWS: ${{ needs.plan.outputs.workflows }}\n"
        "        run: |\n"
        # One failed dispatch (API blip, rate limit) must not strand every
        # later board's badge: keep going, then fail once at the end.
        "          failed=()\n"
        "          for wf in $(echo \"$WORKFLOWS\" | jq -r '.[]'); do\n"
        "            if gh workflow run \"$wf\" --ref \"$GITHUB_REF_NAME\" \\\n"
        "              -f fbuild-run-id=\"$GITHUB_RUN_ID\" -f checkout_ref=\"$GITHUB_SHA\"; then\n"
        "              echo \"dispatched $wf\"\n"
        "            else\n"
        "              echo \"::error::failed to dispatch $wf\"\n"
        "              failed+=(\"$wf\")\n"
        "            fi\n"
        "          done\n"
        "          if [ \"${#failed[@]}\" -gt 0 ]; then\n"
        "            echo \"failed to dispatch: ${failed[*]}\" >&2\n"
        "            exit 1\n"
        "          fi\n"
    )
    return header + render_fbuild_bin_job(
        "plan", "    if: needs.plan.outputs.workflows != '[]'\n", "${{ github.sha }}"
    ) + dispatch


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
        new = new[: new.index("\njobs:\n") + 1] + render_board_jobs(board, families)
        write_if_changed(path, new, args.check, drift, updated)

    write_if_changed(NIGHTLY_PATH, render_nightly(boards), args.check, drift, updated)
    write_if_changed(MINIMAL_PATH, render_ci(boards, "minimal", families), args.check, drift, updated)
    write_if_changed(TEST_PATH, render_ci(boards, "test", families), args.check, drift, updated)
    write_if_changed(FULL_PATH, render_ci(boards, "full", families), args.check, drift, updated)

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

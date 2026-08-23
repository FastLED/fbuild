#!/usr/bin/env python3
"""Require an auto-cancel `concurrency:` block on PR-triggered workflows.

Without one, every push to a feature branch queues a *fresh* copy of every
matching workflow while the superseded runs keep burning runner minutes. With
~80 per-board build workflows that is unbounded back-pressure on the GHA
queue: a two-line typo fix can cost as much as the original push.

The rule enforced here:

  * any workflow with a `pull_request` trigger MUST declare a top-level
    `concurrency:` block whose `cancel-in-progress` is true for PR events;
  * the group MUST vary per event so that non-PR runs are not funnelled into
    one shared group. GitHub keeps at most ONE *pending* run per group, so a
    shared group would silently DROP queued pushes to main -- exactly the SHAs
    that populate the soldr caches and feed the release flow.

Workflows may opt out by listing themselves in EXEMPT with a reason.

Per-board `build-*.yml` files get their block from ci/render_workflows.py;
this script is the backstop for everything else. Run with no arguments:

    uv run --no-project python ci/check_workflow_concurrency.py
"""

from __future__ import annotations

import sys
from pathlib import Path

try:
    import yaml
except ModuleNotFoundError:  # pragma: no cover - CI installs pyyaml
    print(
        "pyyaml is required: uv run --with pyyaml --no-project python "
        "ci/check_workflow_concurrency.py",
        file=sys.stderr,
    )
    raise SystemExit(1)

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"

# name -> why it is allowed to run without auto-cancel.
EXEMPT: dict[str, str] = {
    # Reusable workflows: inside a called workflow the `github` context is the
    # CALLER's, so `github.workflow`/`github.ref` collide across every one of
    # nightly-platforms' ~80 template invocations. A group here would make the
    # boards cancel each other. The calling workflow owns the group instead.
    "template_build.yml": "reusable workflow -- github context belongs to the caller",
    "template_native_build.yml": "reusable workflow -- github context belongs to the caller",
    # Real hardware. Cancelling mid-flash can leave a board in a wedged
    # bootloader state that the next run has to recover from, which costs more
    # than the runner minutes saved. It is label-gated and rare anyway.
    "hw-ci.yml": "self-hosted hardware -- cancelling mid-flash can wedge a board",
    # Fires once, on `pull_request: [opened]`. There is no such thing as a
    # superseded run, and cancelling would drop the PR off the project board.
    "add-to-project.yml": "runs once on PR open -- never superseded",
}

# `on:` unquoted parses as the YAML boolean True, not the string "on".
ON_KEYS = ("on", True)


def load_on_block(doc: dict) -> object:
    for key in ON_KEYS:
        if key in doc:
            return doc[key]
    return None


def has_pull_request_trigger(on_block: object) -> bool:
    if isinstance(on_block, dict):
        return "pull_request" in on_block or "pull_request_target" in on_block
    if isinstance(on_block, list):
        return "pull_request" in on_block or "pull_request_target" in on_block
    return on_block in ("pull_request", "pull_request_target")


def check_file(path: Path) -> list[str]:
    """Return a list of problems with one workflow file (empty == OK)."""
    doc = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(doc, dict):
        return [f"{path.name}: not a YAML mapping"]

    if not has_pull_request_trigger(load_on_block(doc)):
        return []
    if path.name in EXEMPT:
        return []

    conc = doc.get("concurrency")
    if conc is None:
        return [
            f"{path.name}: has a `pull_request` trigger but no top-level "
            f"`concurrency:` block -- superseded PR runs will queue instead "
            f"of cancelling"
        ]
    if not isinstance(conc, dict):
        return [f"{path.name}: `concurrency:` must be a mapping with `group:`"]

    problems: list[str] = []
    group = str(conc.get("group", ""))
    cancel = str(conc.get("cancel-in-progress", "")).lower()

    if "github.run_id" not in group and "false" not in cancel:
        problems.append(
            f"{path.name}: concurrency group {group!r} does not vary by "
            f"`github.run_id` for non-PR events -- a burst of pushes to main "
            f"would drop queued SHAs (GitHub keeps only one pending run per "
            f"group)"
        )
    if "true" not in cancel and "pull_request" not in cancel:
        problems.append(
            f"{path.name}: `cancel-in-progress: {conc.get('cancel-in-progress')!r}` "
            f"never cancels PR runs -- either enable it or add the workflow to "
            f"EXEMPT in ci/check_workflow_concurrency.py with a reason"
        )
    return problems


def main() -> int:
    problems: list[str] = []
    for path in sorted(WORKFLOWS.glob("*.yml")):
        problems.extend(check_file(path))

    if problems:
        print("Workflow concurrency check failed:\n", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(
            "\nAdd this block below `on:` (substituting the file name):\n\n"
            "concurrency:\n"
            "  group: <workflow-file>.yml-${{ github.event_name == "
            "'pull_request' && github.ref || github.run_id }}\n"
            "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n",
            file=sys.stderr,
        )
        return 1

    print("OK: every pull_request-triggered workflow auto-cancels superseded runs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

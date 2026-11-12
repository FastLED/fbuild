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

The same pending-run rule silently starves a NIGHTLY, which is worse than a
dropped push (a push run is rebuilt by the next merge; a lost night is a hole in
`benchmark-stats/history.jsonl` forever):

  * a workflow with a `schedule:` trigger that declares a `concurrency:` group
    MUST scope that group by `github.event_name`, so the cron run never shares
    a group with push or PR runs. `cancel-in-progress: false` does NOT help --
    it protects the running run and nothing else.

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


def has_schedule_trigger(on_block: object) -> bool:
    if isinstance(on_block, dict):
        return "schedule" in on_block
    if isinstance(on_block, list):
        return "schedule" in on_block
    return on_block == "schedule"


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

    on_block = load_on_block(doc)
    problems = check_pull_request_rules(path, doc, on_block)
    problems.extend(check_schedule_rules(path, doc, on_block))
    problems.extend(check_pages_rules(path, doc))
    return problems


def check_pull_request_rules(path: Path, doc: dict, on_block: object) -> list[str]:
    """PR runs must cancel their superseded predecessors."""
    if not has_pull_request_trigger(on_block):
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


def check_schedule_rules(path: Path, doc: dict, on_block: object) -> list[str]:
    """A scheduled run must not be cancellable by another event's run.

    GitHub keeps at most one *pending* run per concurrency group, and
    `cancel-in-progress: false` only protects the run that is already executing.
    A nightly that shares its group with push-to-main runs is therefore replaced
    the instant a merge arrives: on 2026-11-12 the nightly 43190818367 queued at
    09:21:59Z was cancelled two seconds after the push run for the merge of #2004
    was created (10:28:43Z), losing that night's measurement. A workflow with a
    `schedule:` trigger either declares no group at all (nothing can evict it,
    which is what nightly-platforms.yml does) or scopes its group by
    a group scoped by `github.event_name` (one slot per trigger) or one that is
    unique per run via `github.run_id`.
    """
    if not has_schedule_trigger(on_block):
        return []
    if path.name in EXEMPT:
        return []

    conc = doc.get("concurrency")
    if conc is None:
        return []
    if not isinstance(conc, dict):
        return [f"{path.name}: `concurrency:` must be a mapping with `group:`"]

    group = str(conc.get("group", ""))
    # `github.run_id` already makes the group unique per run (nothing can evict
    # such a run); `github.event_name` scopes it per trigger.
    if "github.event_name" in group or "github.run_id" in group:
        return []
    return [
        f"{path.name}: the scheduled run shares concurrency group {group!r} with "
        f"the other triggers -- GitHub keeps only ONE pending run per group, so "
        f"the nightly is cancelled (not queued) whenever a push run arrives. "
        f"`cancel-in-progress: false` does not protect a pending run; scope the "
        f"group by `github.event_name`, e.g. ${{{{ github.event_name == "
        f"'schedule' && 'nightly' || github.ref }}}}"
    ]


# GitHub Pages allows ONE in-flight deployment per environment. A deployment
# job therefore has to serialise on a *shared* group: a group that varies per
# run (or per ref) hands every run its own slot and serialises nothing.
DEPLOY_ACTION_PREFIX = "actions/deploy-pages"
PER_RUN_GROUP_TOKENS = (
    "github.run_id",
    "github.run_number",
    "github.sha",
    "github.head_ref",
    "github.ref",
)


def job_deploys_pages(job: object) -> bool:
    """True when a job runs the Pages deployment action (not just the upload)."""
    if not isinstance(job, dict):
        return False
    steps = job.get("steps")
    if not isinstance(steps, list):
        return False
    return any(
        isinstance(step, dict)
        and str(step.get("uses", "")).startswith(DEPLOY_ACTION_PREFIX)
        for step in steps
    )


def check_pages_rules(path: Path, doc: dict) -> list[str]:
    """A Pages deployment job must serialise on one shared concurrency group.

    `benchmark-build-comparison.yml` publishes from two slots that are allowed to
    overlap: the nightly owns its own `concurrency:` group and a push run keeps
    the per-ref one (FastLED/fbuild#2005), because GitHub cancels a queued run
    when a new one arrives in the same group. The history union and the leased
    push (#2006/#2007) make overlapping *publishers* safe, but the Pages
    deployment itself is not: GitHub allows one deployment in progress per
    environment, and the run that loses that race fails the `deploy-pages` step
    with "Deployment failed, try again later" (actions/deploy-pages#406, #418).
    GitHub's own Pages template prevents this with
    `concurrency: {group: github-pages, cancel-in-progress: false}`; this rule
    makes that structural instead of tribal knowledge.

    A job that only UPLOADS the artifact (`actions/upload-pages-artifact`) is
    not a deployment and is unaffected.
    """
    jobs = doc.get("jobs")
    if not isinstance(jobs, dict):
        return []

    problems: list[str] = []
    for name, job in jobs.items():
        if not job_deploys_pages(job):
            continue
        conc = job.get("concurrency")
        if conc is None:
            problems.append(
                f"{path.name}: job {name!r} runs {DEPLOY_ACTION_PREFIX} but has no "
                f"job-level `concurrency:` block -- two overlapping runs reach it "
                f"and the loser fails with 'Deployment failed, try again later'. "
                f"Add `concurrency: {{group: github-pages, "
                f"cancel-in-progress: false}}`"
            )
            continue
        if not isinstance(conc, dict):
            problems.append(
                f"{path.name}: job {name!r} `concurrency:` must be a mapping "
                f"with `group:`"
            )
            continue
        group = str(conc.get("group", ""))
        if not group:
            problems.append(
                f"{path.name}: job {name!r} `concurrency:` needs a non-empty "
                f"`group:` -- use a shared name such as `github-pages`"
            )
        elif any(token in group for token in PER_RUN_GROUP_TOKENS):
            problems.append(
                f"{path.name}: job {name!r} deploy-pages group {group!r} varies "
                f"per run, so it serialises nothing -- use one shared group "
                f"(e.g. `github-pages`)"
            )
        cancel = str(conc.get("cancel-in-progress", "")).lower()
        if "true" in cancel:
            problems.append(
                f"{path.name}: job {name!r} sets `cancel-in-progress: true` on a "
                f"deploy-pages job -- cancelling a deployment mid-flight can leave "
                f"the Pages environment stuck ('Deployment failed, try again "
                f"later'); let deployments queue instead"
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

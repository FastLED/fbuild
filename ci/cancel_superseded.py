"""Cancel merge-queue runs a newer queue run for the same PR replaced (GEN-022, ci.yml#409).

    python3 ci/cancel_superseded.py [--dry-run]

Runs as a job of every `merge_group` CI run (GITHUB_RUN_ID, GITHUB_REPOSITORY
and a GH_TOKEN with actions: write). A queue ref carries the base commit,
`gh-readonly-queue/main/pr-<N>-<sha>`, so the workflow's concurrency group
never pairs a PR's rebuilt queue run with its old one. When main moves,
GitHub starts a new run per queued PR and the old ones keep holding runners.
The newest queue run for a PR is the live one, so every older in-flight
queue run for the same PR is cancelled. Ported from obs-rust/obs-studio's
`ci/cancel_superseded.py` (#986 there) via talos-hq/clud (#2048).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from datetime import datetime, timezone
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

# The base branch sits between the prefix and `/pr-<N>-<sha>` and may itself
# contain slashes (`gh-readonly-queue/release/2.x/pr-7-abc123`), so the base
# segment cannot be `[^/]+`: that only matched single-segment bases and
# silently classified a slashed base as "not a queue run".
QUEUE_RE = re.compile(r"(?:^|/)gh-readonly-queue/.+/pr-(\d+)-[0-9a-f]+$")
LIVE = ("queued", "in_progress", "waiting", "pending", "requested")


def queue_pr(ref: str) -> int | None:
    """The PR number of a merge-queue ref, else None."""
    match = QUEUE_RE.search(ref)
    return int(match.group(1)) if match else None


def created_at(run: dict) -> datetime:
    """A run's creation time as an aware UTC datetime, or ValueError."""
    raw = run.get("created_at")
    if not isinstance(raw, str) or not raw:
        raise ValueError(f"run {run.get('id')!r} carries no created_at timestamp")
    try:
        # `Z` is accepted from 3.11 on; normalize anyway so the parse cannot
        # depend on the interpreter's tolerance.
        stamp = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(
            f"run {run.get('id')!r}: unparseable created_at {raw!r}"
        ) from error
    return stamp if stamp.tzinfo else stamp.replace(tzinfo=timezone.utc)


def order(run: dict) -> tuple[datetime, int]:
    """Total order over runs: creation time, then id.

    Timestamps are compared as DATETIMES, never as strings: GitHub sometimes
    emits fractional seconds (`...00.500Z`), and as strings `.` sorts before
    `Z`, so a run created 500 ms AFTER a whole-second one reads as older and
    would be cancelled -- the live queue run, not the stale one. Equal times
    (coarse or truncated precision) fall back to the id, which increases with
    creation, so a same-second pair still cancels the earlier run.
    """
    return (created_at(run), int(run["id"]))


def superseded(runs: list[dict], current: dict) -> list[int]:
    """Ids of in-flight queue runs for current's PR that started before it."""
    pr = queue_pr(current.get("head_branch") or "")
    if pr is None:
        return []
    cutoff = order(current)
    return sorted(
        {
            run["id"]
            for run in runs
            if run["id"] != current["id"]
            and run.get("status") in LIVE
            and queue_pr(run.get("head_branch") or "") == pr
            and order(run) < cutoff
        }
    )


def api(path: str, method: str = "GET") -> dict:
    request = Request(
        f"https://api.github.com/{path}",
        method=method,
        headers={
            "Authorization": f"Bearer {os.environ['GH_TOKEN']}",
            "Accept": "application/vnd.github+json",
        },
    )
    with urlopen(request, timeout=30) as response:
        body = response.read()
    return json.loads(body) if body else {}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    repo = os.environ["GITHUB_REPOSITORY"]
    current = api(f"repos/{repo}/actions/runs/{os.environ['GITHUB_RUN_ID']}")
    workflow = current["workflow_id"]
    runs: list[dict] = []
    for status in LIVE:
        listing = api(
            f"repos/{repo}/actions/workflows/{workflow}/runs"
            f"?event=merge_group&status={status}&per_page=100"
        )
        runs += listing["workflow_runs"]
    stale = superseded(runs, current)
    pr = queue_pr(current.get("head_branch") or "")
    if pr is None:
        # The job is wired to merge_group, whose refs are always
        # `gh-readonly-queue/<base>/pr-<N>-<sha>`. Anything else means the event
        # wiring broke, and "cancelled 0" would report success for a cleanup
        # that never had a target -- fail loudly instead of silently passing.
        print(
            f"error: triggering run {current['id']} is not a merge-queue run "
            f"(head_branch={current.get('head_branch')!r}); nothing to cancel",
            file=sys.stderr,
        )
        return 1
    print(f"PR #{pr}: cancelling {len(stale)} superseded queue run(s): {stale}")
    for run_id in stale:
        if args.dry_run:
            continue
        try:
            api(f"repos/{repo}/actions/runs/{run_id}/cancel", method="POST")
        except HTTPError as error:
            # A run that finished meanwhile can no longer be cancelled (409).
            print(f"run {run_id}: cancel returned {error.code}; leaving it")
        except URLError as error:
            # Cleanup is best-effort and must never block the merge queue: a
            # network blip while cancelling leaves a redundant run holding a
            # runner, which is cheaper than a red merge_group check.
            print(f"run {run_id}: cancel failed ({error.reason}); leaving it")
    return 0


if __name__ == "__main__":
    sys.exit(main())

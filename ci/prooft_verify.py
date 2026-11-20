"""Decide which hosted jobs a PR or queue head may skip using the base policy."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import cast

from ci.cancel_superseded import queue_pr

ROOT = Path(__file__).resolve().parents[1]
PROOFT_SOURCE = "git+https://github.com/forge-ci/prooft@d08d58bebcd8592e4526b9bde9ca4717966a8786"
JOBS = ("policy", "dylint", "gate", "linux", "fmt")
FORCE_TAGS = ("[ci-full]", "[ci-remote]", "[no-attest]")


def run_all(reason: str) -> int:
    print(f"prooft: nothing skips ({reason})")
    lines = "".join(f"skip_{job}=false\n" for job in JOBS)
    print(lines, end="")
    if out := os.environ.get("GITHUB_OUTPUT"):
        with open(out, "a", encoding="utf-8") as handle:
            handle.write(lines)
    return 0


def prooft_argv(*args: str, root: Path = ROOT) -> list[str]:
    return ["uvx", "--from", PROOFT_SOURCE, "prooft", "-C", str(root), *args]


def fetch(sha: str) -> bool:
    return subprocess.run(["git", "fetch", "-q", "origin", sha], cwd=ROOT, check=False).returncode == 0


def plan(*args: str) -> int:
    code = subprocess.run(prooft_argv(*args, "--github-output"), check=False).returncode
    return 0 if code == 0 else run_all(f"verifier exited {code}")


def pull_request(base: str, head: str, title: str) -> int:
    if os.environ.get("FORK") == "true" or os.environ.get("EXTENDED") == "true":
        return run_all("forks and expanded CI run remotely")
    if not fetch(head):
        return run_all(f"could not fetch head {head[:12]}")
    return plan("plan", "--base", base, "--head", head, "--title", title)


def queue_pr_head(ref: str, repository: str) -> str:
    # This is GitHub's read-only merge-group ref. Resolve the corresponding
    # PR through GitHub, including trust metadata prooft queue does not take.
    number = queue_pr(ref)
    if number is None or not repository:
        raise ValueError("missing GitHub merge-queue PR identity")
    result = subprocess.run(
        ["gh", "api", f"repos/{repository}/pulls/{number}", "--jq",
         '[.head.sha, .head.repo.full_name, .title, .state, ([.labels[].name] | any(. == "ci-full" or . == "ci-test"))]'],
        cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True,
    )
    head, repo, title, state, extended = cast(tuple[str, str, str, str, bool], json.loads(result.stdout))
    if repo != repository or state != "open" or extended or any(tag in title.lower() for tag in FORCE_TAGS):
        raise ValueError("queue PR requires hosted validation")
    return head


def queue(base: str, head: str) -> int:
    pr_head = queue_pr_head(os.environ.get("QUEUE_REF", ""), os.environ.get("GITHUB_REPOSITORY", ""))
    if not re.fullmatch(r"[0-9a-f]{40}", pr_head) or not fetch(pr_head) or not fetch(head):
        return run_all("could not fetch the queue or PR head")
    parents = subprocess.run(
        ["git", "show", "-s", "--format=%P", head], cwd=ROOT,
        check=True, stdout=subprocess.PIPE, text=True,
    ).stdout.split()
    if len(parents) == 2 and parents[1] != pr_head:
        return run_all("the PR has moved since the merge group was built")
    # The pinned prooft currently declines MERGE-method heads. Keep the real
    # history intact so unsupported topology falls back to hosted jobs.
    return plan("queue", "--base", base, "--pr-head", pr_head, "--queue-head", head)


def main() -> int:
    event = os.environ.get("EVENT_NAME", "")
    base = os.environ.get("BASE_SHA", "")
    head = os.environ.get("HEAD_SHA", "")
    if event not in {"pull_request", "merge_group"}:
        return run_all(f"{event or 'this event'} runs everything")
    if not all(re.fullmatch(r"[0-9a-f]{40}", sha) for sha in (base, head)):
        return run_all(f"{event} without valid base/head shas")
    try:
        if event == "pull_request":
            return pull_request(base, head, os.environ.get("PR_TITLE", ""))
        return queue(base, head)
    except (OSError, subprocess.CalledProcessError, ValueError, TypeError) as error:
        return run_all(str(error))


if __name__ == "__main__":
    sys.exit(main())

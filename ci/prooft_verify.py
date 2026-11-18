"""ci-minimal's `prooft` job: decide which jobs this run may skip (stdlib only).

    python3 -P ci/prooft_verify.py

Adapted from forge-ci/prooft's own `ci/verify.py`. The job checks out the
*base* (the PR base), so this script, the pinned prooft revision below and
`ci-attestations.yml` all come from the base: a PR cannot edit what judges
it. The PR head is fetched only as data.

- ``pull_request``: ``prooft plan`` on the PR head. A job is skipped only
  when every gate it covers carries a fresh, valid `Ci-Attestation:`
  trailer (written by ``./push``) and prooft trusts the head.
- anything else (merge queue, main pushes, dispatches) and fork PRs skip
  nothing. fbuild's queue merges with the MERGE method, whose two-parent
  queue heads prooft never credits, and the push to main is what keeps a
  forged trailer from going unnoticed.

Every failure path prints why and emits ``skip_<job>=false``: a broken
verifier costs CI time, never coverage.
"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
# forge-ci/prooft 0.2.0 + data/swift ecosystems. Bump deliberately: the base
# decides which verifier judges a PR.
PROOFT_SOURCE = "git+https://github.com/forge-ci/prooft@d08d58bebcd8592e4526b9bde9ca4717966a8786"
# The dylint.yml jobs ci-minimal's single Dylint call runs; the call is
# skipped only when prooft credits all three (ci-attestations.yml `jobs`).
JOBS = ("policy", "dylint", "gate")


def write_outputs(values: dict[str, str]) -> None:
    lines = "".join(f"{k}={v}\n" for k, v in values.items())
    print(lines, end="")
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a", encoding="utf-8") as handle:
            handle.write(lines)


def run_all(reason: str) -> int:
    print(f"prooft: nothing skips ({reason})")
    write_outputs({f"skip_{job}": "false" for job in JOBS})
    return 0


def prooft_argv(*args: str) -> list[str]:
    return ["uvx", "--from", PROOFT_SOURCE, "prooft", "-C", str(ROOT), *args]


def fetch(sha: str) -> bool:
    return subprocess.run(["git", "fetch", "-q", "origin", sha], cwd=ROOT, check=False).returncode == 0


def pull_request(base: str, head: str, title: str) -> int:
    if os.environ.get("FORK") == "true":
        return run_all("fork PRs always run remotely")
    if not fetch(head):
        return run_all(f"could not fetch head {head[:12]}")
    argv = prooft_argv("plan", "--base", base, "--head", head, "--title", title, "--github-output")
    code = subprocess.run(argv, check=False).returncode
    return 0 if code == 0 else run_all(f"verifier exited {code}")


def main() -> int:
    event = os.environ.get("EVENT_NAME", "")
    base = os.environ.get("BASE_SHA", "")
    head = os.environ.get("HEAD_SHA", "")
    if event != "pull_request":
        return run_all(f"{event or 'this event'} runs everything")
    if not base or not head:
        return run_all("pull_request without base/head shas")
    return pull_request(base, head, os.environ.get("PR_TITLE", ""))


if __name__ == "__main__":
    sys.exit(main())

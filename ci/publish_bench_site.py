#!/usr/bin/env python3
"""Publish the benchmark site onto the ``benchmark-stats`` branch, without loss.

`benchmark-build-comparison.yml` has TWO publishers that can now overlap: the
nightly cron owns its own `concurrency:` group and a push run keeps the per-ref
group (FastLED/fbuild#2005). The publisher therefore has to survive a second
publisher landing between its own fetch and its own push.

The history union in ``ci/merge_bench_history.py`` only helps when the published
copy is read AFTER the rival's push. A plain ``git push --force`` closes the
window in the worst possible way: it cannot fail, so it silently reverts the
rival's row -- `history.jsonl` is append-only, so that night is a permanent hole.
Reproduced with two real publishers against one bare remote: after A pushed its
row, B (which had snapshotted the branch before A's push) force-pushed its own
tree and the final history held seed rows + B only, A's row gone.

The fix is a lease plus a bounded retry:

  * every attempt re-reads the published branch FIRST, so the union input is
    whatever is published now, not what was published when the run started;
  * the push is `--force-with-lease=refs/heads/<branch>:<sha-we-read>` (an empty
    expected value means "the branch must not exist yet"), so a rival's push
    turns into a rejected push instead of a lost row;
  * a rejection simply re-assembles from the newer branch and pushes again --
    idempotent, because the union de-duplicates by ``(ts, sha)``;
  * after `--max-attempts` the step fails loudly rather than clobbering.

Unioning ``history.jsonl`` is only half of it: the rest of the site tree
(``index.html``, ``benchmark.svg``, ``latest.json``) describes exactly ONE run.
The nightly now owns its own concurrency slot (#2005), so the run that finishes
LAST is not always the run that measured LAST -- a nightly that started at 09:17Z
can publish after a later merge's push run and would then hand the site an older
``latest.json`` than the one already published. The measurement overlay is
therefore recency-gated on ``metadata.generated_at``: an older report never
replaces a newer one, while its history row is still unioned and ``site/mirror``
(which is checked-out content, always current) is still published.

Usage (from the repository root, so ``ci`` is importable as a package):

    python3 -m ci.publish_bench_site \\
        --repo-url <https remote> --publish-dir <empty temp dir> \\
        --measurements benchmark-measurements --mirror-dir repo/site/mirror \\
        --message "chore: publish build benchmark for <sha>"
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from ci.merge_bench_history import merge_history

MAX_ATTEMPTS = 5
HISTORY = "history.jsonl"
LATEST = "latest.json"
# Every report is stamped by the harness with `date -u +'%Y-%m-%dT%H:%M:%SZ'`,
# so the comparison is a parse of that one format; anything else is unknown.
REPORT_TIMESTAMP = "%Y-%m-%dT%H:%M:%SZ"
# A rejected push means the branch moved under us; anything else is a real
# failure (auth, network) and retrying would only burn the budget.
RETRYABLE = ("stale info", "non-fast-forward", "cannot lock ref", "failed to push")


class PublishError(RuntimeError):
    """The branch could not be published, or git refused for a real reason."""


@dataclass(frozen=True)
class PublishSpec:
    """Everything one publish needs; paths may be absent (mirror-only publish)."""

    repo_url: str
    publish_dir: Path
    branch: str = "benchmark-stats"
    measurements: Path | None = None
    mirror_dir: Path | None = None
    message: str = "chore: publish benchmark site"
    max_attempts: int = MAX_ATTEMPTS


def git(cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    """Run git in `cwd`; raise on failure unless the caller wants the verdict."""
    result = subprocess.run(
        ["git", "-C", str(cwd), *args], capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        raise PublishError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result


def ensure_repo(spec: PublishSpec) -> None:
    """A bare temp dir becomes a local repo with an identity (idempotent)."""
    spec.publish_dir.mkdir(parents=True, exist_ok=True)
    if not (spec.publish_dir / ".git").exists():
        git(spec.publish_dir, "init", "--quiet")
    git(spec.publish_dir, "config", "user.name", "github-actions[bot]")
    git(
        spec.publish_dir,
        "config",
        "user.email",
        "41898282+github-actions[bot]@users.noreply.github.com",
    )


def remote_head(spec: PublishSpec) -> str | None:
    """The published branch's sha, or None when the branch does not exist yet."""
    result = git(
        spec.publish_dir,
        "ls-remote",
        "--exit-code",
        spec.repo_url,
        f"refs/heads/{spec.branch}",
        check=False,
    )
    if result.returncode != 0:
        return None
    fields = result.stdout.split()
    return fields[0] if fields else None


def read_published(spec: PublishSpec) -> str | None:
    """Point `publish_dir` at the published tip; return the sha it was read at."""
    head = remote_head(spec)
    if head is not None:
        git(
            spec.publish_dir,
            "fetch",
            "--depth=1",
            spec.repo_url,
            f"refs/heads/{spec.branch}",
        )
        git(spec.publish_dir, "reset", "--hard", "FETCH_HEAD")
    git(spec.publish_dir, "checkout", "-B", spec.branch)
    return head


def read_history(spec: PublishSpec) -> str:
    """The published history, captured BEFORE the overlay overwrites it."""
    path = spec.publish_dir / HISTORY
    return path.read_text(encoding="utf-8") if path.is_file() else ""


def copy_tree(source: Path, destination: Path) -> None:
    shutil.copytree(source, destination, dirs_exist_ok=True)


def report_timestamp(directory: Path) -> datetime | None:
    """`metadata.generated_at` of the report in `directory`, or None if unknown."""
    try:
        report = json.loads((directory / LATEST).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    metadata = report.get("metadata") if isinstance(report, dict) else None
    raw = metadata.get("generated_at") if isinstance(metadata, dict) else None
    if not isinstance(raw, str):
        return None
    try:
        return datetime.strptime(raw, REPORT_TIMESTAMP)
    except ValueError:
        return None


def report_is_older(published: Path, fresh: Path) -> bool:
    """True when `fresh` measured before the report already on the branch."""
    fresh_at = report_timestamp(fresh)
    published_at = report_timestamp(published)
    if fresh_at is None or published_at is None:
        # Unknown on either side: fall back to "publish it", never to silence.
        return False
    return fresh_at <= published_at


def overlay(spec: PublishSpec, published_history: str) -> None:
    """Overlay this run's measurements and mirror, unioning history."""
    if spec.measurements is not None and spec.measurements.is_dir():
        if report_is_older(spec.publish_dir, spec.measurements):
            print(
                "Published report is newer than this run's; keeping the published "
                "site and unioning history only."
            )
        else:
            copy_tree(spec.measurements, spec.publish_dir)
        fresh = spec.measurements / HISTORY
        if fresh.is_file():
            merged = merge_history(published_history, fresh.read_text(encoding="utf-8"))
            (spec.publish_dir / HISTORY).write_text(merged, encoding="utf-8")
    else:
        print("No benchmark measurements this run; keeping the last published report.")
    if spec.mirror_dir is not None and spec.mirror_dir.is_dir():
        copy_tree(spec.mirror_dir, spec.publish_dir / "mirror")


def stage_and_commit(spec: PublishSpec) -> bool:
    """Commit the assembled tree; False when it is byte-identical to published."""
    git(spec.publish_dir, "add", "-A")
    if (
        git(spec.publish_dir, "diff", "--cached", "--quiet", check=False).returncode
        == 0
    ):
        print("No site changes to publish.")
        return False
    git(spec.publish_dir, "commit", "--message", spec.message, "--quiet")
    return True


def push(spec: PublishSpec, expected: str | None) -> tuple[bool, str]:
    """Push under a lease on the sha we read; a moved ref is a rejection, not loss."""
    lease = f"--force-with-lease=refs/heads/{spec.branch}:{expected or ''}"
    result = git(
        spec.publish_dir,
        "push",
        lease,
        spec.repo_url,
        f"HEAD:refs/heads/{spec.branch}",
        check=False,
    )
    if result.returncode == 0:
        return True, result.stdout
    return False, f"{result.stderr}\n{result.stdout}"


def publish(spec: PublishSpec, before_push=None) -> int:
    """Assemble, commit and push, re-reading the branch after every rejection."""
    ensure_repo(spec)
    for attempt in range(1, spec.max_attempts + 1):
        head = read_published(spec)
        overlay(spec, read_history(spec))
        if not stage_and_commit(spec):
            return 0
        if before_push is not None:
            before_push(attempt)
        pushed, output = push(spec, head)
        if pushed:
            print(
                f"published {spec.branch} on attempt {attempt} (was {head or 'absent'})"
            )
            return 0
        if not any(marker in output for marker in RETRYABLE):
            raise PublishError(f"push refused: {output.strip()}")
        print(f"attempt {attempt}: {spec.branch} moved while assembling; re-reading")
    raise PublishError(
        f"could not publish {spec.branch} after {spec.max_attempts} attempts"
    )


def parse_args(argv: list[str] | None = None) -> PublishSpec:
    parser = argparse.ArgumentParser(description="Publish the benchmark site branch.")
    parser.add_argument("--repo-url", required=True)
    parser.add_argument("--publish-dir", type=Path, required=True)
    parser.add_argument("--measurements", type=Path, default=None)
    parser.add_argument("--mirror-dir", type=Path, default=None)
    parser.add_argument("--branch", default="benchmark-stats")
    parser.add_argument("--message", default="chore: publish benchmark site")
    parser.add_argument("--max-attempts", type=int, default=MAX_ATTEMPTS)
    args = parser.parse_args(argv)
    return PublishSpec(
        repo_url=args.repo_url,
        publish_dir=args.publish_dir,
        branch=args.branch,
        measurements=args.measurements,
        mirror_dir=args.mirror_dir,
        message=args.message,
        max_attempts=args.max_attempts,
    )


def main(argv: list[str] | None = None) -> int:
    try:
        return publish(parse_args(argv))
    except PublishError as error:
        print(f"publish failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

"""Did Cargo.lock, uv.lock, or rust-toolchain.toml change vs the parent commit?

CACHE-034 (zackees/ci.yml#352/#354/#360): `[flow.main] pre-prune = true` in
ci.toml waives the lockfile-change peak from CACHE-004's worst case, and the
waiver is only honoured when a real `ci-lint cache preprune` runs ahead of the
cache saves. The prune should spend its (forecasting, entry-deleting) work only
on a writer push that actually changed one of these files -- comparing against
every ordinary code-only push would be waste. "Parent commit" is deliberately
simple (`HEAD^`, one commit back on whatever ref this job checked out); the
checkout step needs `fetch-depth: 2` for `HEAD^` to exist locally.

Convention copied from zackees/template-python-rust-cmd's script of the same
name (the fleet's reference honours of the pre-prune waiver). A watched file
that simply does not exist in this repository never matches, so the extra
entries are harmless.

Prints `lockfile-changed: <bool>` and, when GITHUB_OUTPUT is set, writes
`changed=true|false` for the workflow step's `steps.lockfile.outputs.changed`,
plus `args=--lockfile-changed` (or an empty value) so a caller can keep the
pre-prune step unconditionally executable -- the gate replay proof
(local-gate.toml's gate.replay) requires every declared step to show an
executed-and-successful section, so a step gated on `changed == 'true'`
would be skipped (and unprovable) on a clean-diff replay.
Exits 0 even when the diff cannot be computed: a shallow/first commit or any
other git error is treated conservatively as "changed" so the pre-prune
forecast runs rather than silently skipping it.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WATCHED = ("Cargo.lock", "uv.lock", "rust-toolchain.toml")


def main() -> int:
    proc = subprocess.run(
        ["git", "diff", "--name-only", "HEAD^", "HEAD", "--", *WATCHED],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        # No parent commit (a shallow/first commit) or another git error --
        # never crash the job over this; treat conservatively as "changed"
        # so preprune's forecast runs rather than silently skipping it.
        print(
            f"ci/lockfile_changed.py: git diff failed (rc={proc.returncode}): "
            f"{proc.stderr.strip()} -- treating as changed (conservative)",
            file=sys.stderr,
        )
        changed = True
    else:
        changed = bool(proc.stdout.strip())

    print(f"lockfile-changed: {changed} (watched: {', '.join(WATCHED)})")
    gh_out = os.environ.get("GITHUB_OUTPUT")
    if gh_out:
        with open(gh_out, "a", encoding="utf-8") as fh:
            fh.write(f"changed={'true' if changed else 'false'}\n")
            fh.write(f"args={'--lockfile-changed' if changed else ''}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
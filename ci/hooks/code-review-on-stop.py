#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Stop hook: triggers code review on session end.

Detects whether source files were changed during this session.
If so, asks Claude to run the /code-review skill before ending.
Uses a marker file to avoid infinite loops (review runs once per session).

Exit codes:
  0 - Always (feedback via stderr)
"""

import hashlib
import json
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent.resolve()
PROJECT_ROOT = SCRIPT_DIR.parent.parent
SESSION_FINGERPRINT_FILE = PROJECT_ROOT / ".cache" / "session_fingerprint.json"
REVIEW_MARKER = PROJECT_ROOT / ".cache" / "code_review_done"


def run_cmd(cmd):
    """Run a command rooted at PROJECT_ROOT.

    FastLED/fbuild#812: 60-second watchdog. Same rationale as
    check-on-start.py — stuck `git status`/`git diff` on Windows would
    otherwise wedge the Stop hook indefinitely.
    """
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        cwd=str(PROJECT_ROOT),
        timeout=60,
    )


def get_current_fingerprint():
    """Get MD5 fingerprint of current git status."""
    result = run_cmd(["git", "status", "--porcelain"])
    if result.returncode != 0 or not result.stdout.strip():
        return None
    return hashlib.md5(result.stdout.encode()).hexdigest()


def get_session_fingerprint():
    """Read fingerprint captured at session start."""
    if SESSION_FINGERPRINT_FILE.exists():
        try:
            data = json.loads(SESSION_FINGERPRINT_FILE.read_text())
            return data.get("fingerprint")
        except Exception:
            return None
    return None


def has_source_changes():
    """Check if .rs or .json files were changed (not just any file)."""
    result = run_cmd(["git", "diff", "--name-only", "HEAD"])
    if result.returncode != 0:
        return False
    staged = run_cmd(["git", "diff", "--name-only", "--cached"])
    all_files = result.stdout + "\n" + (staged.stdout if staged.returncode == 0 else "")
    for line in all_files.strip().splitlines():
        if line.strip().endswith((".rs", ".json", ".toml")):
            return True
    return False


def session_has_changes():
    """Check if changes were made during this session."""
    current_fp = get_current_fingerprint()
    if current_fp is None:
        return False
    session_fp = get_session_fingerprint()
    if session_fp is None:
        # No fingerprint at start = repo was clean; changes now = session changes
        return True
    return current_fp != session_fp


def main():
    # Already reviewed this session — don't loop
    if REVIEW_MARKER.exists():
        return 0

    if not session_has_changes():
        return 0

    if not has_source_changes():
        return 0

    # Mark review as triggered so we don't loop
    REVIEW_MARKER.parent.mkdir(parents=True, exist_ok=True)
    REVIEW_MARKER.write_text("done")

    print(
        "Source files changed during this session. "
        "Please run /code-review to check for: hardcoded values (should be in JSON), "
        "code that belongs in core, board/MCU JSON quality, "
        "orchestrator completeness, and bugs.",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

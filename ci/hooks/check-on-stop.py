#!/usr/bin/env python3
"""Stop hook: runs full workspace lint and tests.

Smart mode: only runs if files were actually changed during this session.
Session fingerprint is captured at session start (check-on-start.py) and
compared here. If nothing changed during the session, everything is skipped.

Exit codes:
  0 - All passed or skipped (no changes during session)
  2 - Lint or test failures (stderr fed back to Claude)
"""

import hashlib
import json
import subprocess
import sys
import threading
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent.resolve()
PROJECT_ROOT = SCRIPT_DIR.parent.parent
SESSION_FINGERPRINT_FILE = PROJECT_ROOT / ".cache" / "session_fingerprint.json"


def run_cmd(cmd):
    """Run a command rooted at PROJECT_ROOT."""
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        cwd=str(PROJECT_ROOT),
    )


def report_failure(label, result):
    print(f"{label}:", file=sys.stderr)
    if result.stdout.strip():
        print(result.stdout.strip(), file=sys.stderr)
    if result.stderr.strip():
        print(result.stderr.strip(), file=sys.stderr)


def get_current_fingerprint():
    """Get MD5 fingerprint of current git status."""
    result = run_cmd(["git", "status", "--porcelain"])
    if result.returncode != 0:
        return None
    status_output = result.stdout
    if not status_output.strip():
        return None
    return hashlib.md5(status_output.encode()).hexdigest()


def get_session_fingerprint():
    """Read fingerprint captured at session start."""
    if SESSION_FINGERPRINT_FILE.exists():
        try:
            data = json.loads(SESSION_FINGERPRINT_FILE.read_text())
            return data.get("fingerprint")
        except Exception:
            return None
    return None


def should_skip():
    """Check if hook should skip based on session fingerprints."""
    current_fp = get_current_fingerprint()

    # No changes at all right now — skip
    if current_fp is None:
        return True

    # Check session fingerprint (captured at session start)
    session_fp = get_session_fingerprint()
    if session_fp is None:
        # No session fingerprint means repo was clean at start;
        # if we have changes now, they were made during this session
        return False

    # Same fingerprint as session start — no changes this session
    if current_fp == session_fp:
        return True

    # Different — changes made this session
    return False


def main():
    if should_skip():
        print("Skipping stop checks (no changes during this session)", file=sys.stderr)
        return 0

    print("Running full workspace checks (changes detected)", file=sys.stderr)

    lint_script = str(PROJECT_ROOT / "lint")
    test_script = str(PROJECT_ROOT / "test")

    # Run lint and tests concurrently
    lint_results = []
    test_results = []

    def do_lint():
        result = run_cmd(["uv", "run", "--script", lint_script, "--fix"])
        lint_results.append(result)

    def do_test():
        result = run_cmd(["uv", "run", "--script", test_script])
        test_results.append(result)

    lint_thread = threading.Thread(target=do_lint)
    test_thread = threading.Thread(target=do_test)
    lint_thread.start()
    test_thread.start()

    # Wait for lint first
    lint_thread.join()
    lint_result = lint_results[0]

    if lint_result.returncode != 0:
        report_failure("Lint failed", lint_result)
        test_thread.join()
        return 2

    # Lint passed — wait for tests
    test_thread.join()
    test_result = test_results[0]

    if test_result.returncode != 0:
        report_failure("Tests failed", test_result)
        return 2

    print("All checks passed", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

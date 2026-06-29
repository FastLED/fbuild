#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""PostToolUse hook: runs per-file lint on edited Rust files.

Delegates to ./lint in single-file mode for speed.
Runs after every Edit/Write on .rs files.

Exit codes:
  0 - Success or non-Rust file
  2 - Lint violations found (stderr fed back to Claude)
"""

import json
import os
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent.resolve()
PROJECT_ROOT = SCRIPT_DIR.parent.parent

sys.path.insert(0, str(SCRIPT_DIR))
from _output import truncate_output  # noqa: E402


def main():
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        return 0

    file_path = data.get("tool_input", {}).get("file_path", "")
    if not file_path:
        return 0

    # Resolve relative paths against project root
    if not os.path.isabs(file_path):
        file_path = os.path.join(str(PROJECT_ROOT), file_path)

    file_path = os.path.realpath(file_path)

    # Only lint Rust files
    if not file_path.endswith(".rs"):
        return 0

    # Skip deleted files
    if not os.path.isfile(file_path):
        return 0

    # Skip files outside this project (e.g. when editing inside a worktree
    # of another repo). detect_crate() would otherwise read the worktree's
    # `crates/<x>` segment and run clippy with -p <x> against fbuild, which
    # either fails or — when names collide — lints the wrong code.
    project_root_real = os.path.normcase(os.path.realpath(str(PROJECT_ROOT)))
    file_path_check = os.path.normcase(file_path)
    if os.path.commonpath([project_root_real, file_path_check]) != project_root_real:
        return 0

    # Delegate to ./lint in single-file mode. Use the active interpreter
    # directly instead of `uv run --script` so we don't trigger another
    # editable-build of the fbuild project (which would re-run
    # `soldr cargo build --release -p fbuild-cli` on every Edit).
    lint_script = str(PROJECT_ROOT / "lint")
    # FastLED/fbuild#812: 10-minute watchdog. A wedged single-file
    # clippy would otherwise block the PostToolUse hook forever.
    result = subprocess.run(
        [sys.executable, lint_script, file_path],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        cwd=str(PROJECT_ROOT),
        timeout=600,
    )

    if result.returncode != 0:
        rel_path = os.path.relpath(file_path, str(PROJECT_ROOT))
        print(f"Lint violations in {rel_path}:", file=sys.stderr)
        if result.stdout.strip():
            print(truncate_output(result.stdout.strip()), file=sys.stderr)
        if result.stderr.strip():
            print(truncate_output(result.stderr.strip()), file=sys.stderr)
        return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())

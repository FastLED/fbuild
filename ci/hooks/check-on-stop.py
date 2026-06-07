#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Stop hook: runs lint and tests scoped to the crates touched this session.

Smart mode: only runs if files were actually changed during this session.
Session fingerprint is captured at session start (check-on-start.py) and
compared here. If nothing changed during the session, everything is skipped.

Scoping rules (issue #465):
  - Cargo.toml / Cargo.lock / rust-toolchain.toml / .cargo/** → workspace-wide
  - changes under crates/<name>/ → per-crate clippy + test for each <name>
  - changes only outside crates/ (and not workspace-wide) → format check only
  - no Rust-relevant changes at all → skip

Exit codes:
  0 - All passed or skipped
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

# Repo-root files whose change forces a workspace-wide lint+test —
# they affect every crate (cross-crate deps, toolchain pin, lint config).
WORKSPACE_TRIGGER_FILES = frozenset({
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rustfmt.toml",
    "clippy.toml",
})
# Path prefixes that also force workspace-wide (config / shared infra).
WORKSPACE_PREFIXES = (".cargo/",)
# Extensions that participate in the lint/test gate. Anything else
# (e.g. .md, .txt, generated JSON outside crates/) doesn't trigger work.
RUST_EXTENSIONS = (".rs",)


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


def get_dirty_files():
    """Return paths of files dirty in the worktree (modified + untracked).

    Uses `git status --porcelain -z` so filenames with spaces or unusual
    characters are handled correctly. Renames are reported with both
    source and destination — we count the destination (the path that
    exists on disk and might compile).
    """
    result = run_cmd(["git", "status", "--porcelain", "-z"])
    if result.returncode != 0 or not result.stdout:
        return []
    out = []
    parts = result.stdout.split("\0")
    i = 0
    while i < len(parts):
        entry = parts[i]
        if not entry:
            i += 1
            continue
        # `XY ` prefix + filename, where XY is two status chars + a space.
        # For renames ("R "), the destination is on this entry and source
        # is the next NUL-separated token; consume both, keep destination.
        if len(entry) < 4:
            i += 1
            continue
        status = entry[:2]
        path = entry[3:]
        if status.startswith("R") or status.startswith("C"):
            # Destination is this entry's path; source is next, skip it.
            i += 2
        else:
            i += 1
        out.append(path)
    return out


def classify_changes(files):
    """Map a list of changed paths to (crates_set, needs_workspace, has_rust).

    - crates_set: distinct crate names under `crates/<name>/...` that changed
    - needs_workspace: True if any change forces a workspace-wide run
      (Cargo.lock, Cargo.toml at root, rust-toolchain.toml, .cargo/**)
    - has_rust: True if any `.rs` file changed anywhere (so even non-crate
      Rust files in benches or examples still get a workspace check)
    """
    crates: set[str] = set()
    needs_workspace = False
    has_rust = False
    for raw in files:
        # Normalize Windows separators for matching.
        path = raw.replace("\\", "/")
        if path in WORKSPACE_TRIGGER_FILES:
            needs_workspace = True
        if any(path.startswith(p) for p in WORKSPACE_PREFIXES):
            needs_workspace = True
        if path.startswith("crates/"):
            parts = path.split("/")
            if len(parts) >= 2 and parts[1]:
                crates.add(parts[1])
        if path.endswith(RUST_EXTENSIONS):
            has_rust = True
    return crates, needs_workspace, has_rust


def run_lint(crates, needs_workspace):
    """Run rustfmt --check + clippy, scoped to the changed crates."""
    # rustfmt is workspace-cheap and catches cross-crate consistency;
    # always run --all. Use --check (no autofix) because Stop is read-only
    # from the user's POV — they shouldn't see surprise modifications.
    fmt = run_cmd(["soldr", "cargo", "fmt", "--all", "--", "--check"])
    if fmt.returncode != 0:
        report_failure("Formatting check failed (run `soldr cargo fmt --all` to fix)", fmt)
        return fmt
    cmd = ["soldr", "cargo", "clippy"]
    if needs_workspace or not crates:
        cmd += ["--workspace"]
    else:
        for c in sorted(crates):
            cmd += ["-p", c]
    cmd += ["--all-targets", "--", "-D", "warnings"]
    return run_cmd(cmd)


def run_tests(crates, needs_workspace):
    """Run cargo test, scoped to the changed crates."""
    cmd = ["soldr", "cargo", "test"]
    if needs_workspace or not crates:
        cmd += ["--workspace"]
    else:
        for c in sorted(crates):
            cmd += ["-p", c]
    return run_cmd(cmd)


def main():
    if should_skip():
        print("Skipping stop checks (no changes during this session)", file=sys.stderr)
        return 0

    dirty = get_dirty_files()
    crates, needs_workspace, has_rust = classify_changes(dirty)

    if not has_rust and not needs_workspace:
        # No Rust-relevant changes (markdown, json outside crates/, etc.)
        print(
            f"Skipping stop checks (changes touched no Rust code: {len(dirty)} non-Rust file(s))",
            file=sys.stderr,
        )
        return 0

    scope_label = (
        "workspace-wide (Cargo.toml/lock/toolchain change)"
        if needs_workspace
        else f"scoped to {len(crates)} crate(s): {', '.join(sorted(crates))}"
        if crates
        else "workspace-wide (no per-crate attribution available)"
    )
    print(f"Running stop checks: {scope_label}", file=sys.stderr)

    lint_results = []
    test_results = []

    def do_lint():
        lint_results.append(run_lint(crates, needs_workspace))

    def do_test():
        test_results.append(run_tests(crates, needs_workspace))

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

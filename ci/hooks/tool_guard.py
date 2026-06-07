#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse hook: blocks bare Rust commands and bare python/pip.

All cargo/rustc/rustfmt must go through a globally-installed `soldr`
(ensures correct toolchain). The historical `uv run soldr ...` path is
also blocked — `soldr` was removed from `ci/dev-tools` as of issue #251
and is no longer available in the repo-local uv env.
All python must go through uv (ensures correct environment).

Exit codes:
  0 - Allow (outputs JSON hookSpecificOutput to deny if needed)
"""

import json
import re
import sys


RUST_TOOLS = {"cargo", "rustc", "rustfmt", "clippy-driver", "cargo-clippy", "cargo-fmt"}
PYTHON_TOOLS = {"python", "python3", "pip", "pip3"}

# Only the bare, global `soldr` is allowed for Rust tooling.
SOLDR_PREFIXES = ("soldr ",)
UV_RUN_PREFIX = "uv run "
UV_PIP_PREFIX = "uv pip "
# `uv run` targets that are forbidden because they belong to the
# global-soldr path: `cargo` / `rustc` / etc. (the console-script shims
# from a venv-installed Rust toolchain) and `soldr` itself (since
# soldr is no longer a venv dependency — see issue #251).
UV_RUN_FORBIDDEN_TARGETS = RUST_TOOLS | {"soldr"}
UV_RUN_FLAGS_WITH_VALUES = {
    "--config-setting",
    "--directory",
    "--env-file",
    "--extra",
    "--find-links",
    "--from",
    "--group",
    "--index-url",
    "--no-extra",
    "--no-group",
    "--only-group",
    "--project",
    "--python",
    "--with",
    "--with-editable",
    "--with-requirements",
    "-m",
    "-p",
}


FORBIDDEN_SCRIPT_DIRS = re.compile(
    r"""(?:^|[\s/\\])      # start or separator
        (?:bench|tests?)   # forbidden directories
        [/\\]              # path separator
        \S*\.py            # any .py file
    """,
    re.VERBOSE,
)

DENY_PYTHON_IN_CODE = (
    "Do not use Python for benchmarks or tests. "
    "Write them in Rust instead. Python is only for CI scripts and packaging."
)


def uv_run_target(parts):
    """Return the uv-run command target after leading uv options."""
    index = 2
    while index < len(parts):
        token = parts[index]
        if token == "--":
            index += 1
            continue
        if token in UV_RUN_FLAGS_WITH_VALUES:
            index += 2
            continue
        if any(token.startswith(f"{flag}=") for flag in UV_RUN_FLAGS_WITH_VALUES):
            index += 1
            continue
        if token.startswith("-"):
            index += 1
            continue
        return token
    return ""


def check_command(command):
    """Check a command string for forbidden bare invocations.

    Returns (tool, reason) if forbidden, None if allowed.
    """
    # ── Global check: block .py scripts in bench/ or tests/ dirs ─────
    # Catches all forms: uv run python bench/x.py, uv run bench/x.py,
    # uv run --script bench/x.py, ./bench/x.py, python tests/x.py, etc.
    if FORBIDDEN_SCRIPT_DIRS.search(command):
        return ("python", DENY_PYTHON_IN_CODE)

    # ── Per-segment checks ───────────────────────────────────────────
    segments = re.split(r"&&|\|\||;", command)

    for seg in segments:
        seg = seg.strip()
        if not seg:
            continue

        # Skip if Rust tooling is explicitly routed through soldr.
        if any(seg.startswith(p) for p in SOLDR_PREFIXES):
            continue

        if seg.startswith(UV_PIP_PREFIX):
            continue

        first_word = seg.split()[0] if seg.split() else ""

        if seg.startswith(UV_RUN_PREFIX):
            parts = seg.split()
            # Block both `uv run cargo ...` (the old venv-shim path) and
            # `uv run soldr ...` (no longer valid since #251 removed
            # soldr from the repo-local uv env).
            run_target = uv_run_target(parts)
            if run_target in UV_RUN_FORBIDDEN_TARGETS:
                if run_target == "soldr":
                    return (
                        "soldr",
                        "Use a globally-installed `soldr ...` instead of "
                        "`uv run soldr ...`. soldr was removed from the "
                        "repo-local uv env in issue #251. Install via "
                        "`uv tool install soldr` (or see "
                        "https://github.com/zackees/soldr).",
                    )
                return (
                    run_target,
                    f"Use `soldr {run_target} ...` instead of "
                    f"`uv run {run_target} ...`. soldr resolves the checked-in "
                    f"Rust toolchain directly via rustup.",
                )
            continue

        if first_word in RUST_TOOLS:
            return (
                first_word,
                f"Use `soldr {first_word} ...` instead of bare "
                f"`{first_word}`. soldr (installed globally) resolves the "
                f"checked-in Rust toolchain directly via rustup.",
            )

        if first_word in PYTHON_TOOLS:
            if first_word.startswith("pip"):
                suggestion = f"uv pip {' '.join(seg.split()[1:])}" if len(seg.split()) > 1 else "uv pip ..."
                return (
                    first_word,
                    f"Use `{suggestion}` instead of bare `{first_word}`. "
                    f"All pip operations must go through uv.",
                )
            return (
                first_word,
                f"Use `uv run ...` instead of bare `{first_word}`. "
                f"All Python must be executed through uv.",
            )

    return None


def deny(reason):
    """Output a JSON deny response."""
    json.dump({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }, sys.stdout)


def extract_command(data):
    """Best-effort extraction across shell tool event shapes."""
    tool_input = data.get("tool_input", {})
    if not isinstance(tool_input, dict):
        return ""
    for key in ("command", "script", "cmd"):
        value = tool_input.get(key)
        if isinstance(value, str) and value.strip():
            return value
    return ""


def main():
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        sys.exit(0)

    tool_name = data.get("tool_name", "")
    if tool_name not in {"Bash", "Shell", "PowerShell"}:
        sys.exit(0)

    command = extract_command(data)
    if not command:
        sys.exit(0)

    result = check_command(command)
    if result:
        _, reason = result
        deny(reason)

    sys.exit(0)


if __name__ == "__main__":
    main()

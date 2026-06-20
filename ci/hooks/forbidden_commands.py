#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.10"
# ///
"""PreToolUse hook: block direct invocation of low-level deploy/debug tools.

Mirror of FastLED's `ci/hooks/check_forbidden_commands.py` —
FastLED/fbuild#694, filed in the wake of FastLED/FastLED#3300 / #3325
/ #3339 (the LPC845-BRK bring-up incident).

## The structural lesson

"Device looks silent" debugging sessions are almost always agents
reaching for a low-level tool that bypasses the project's accumulated
knowledge about that board — pyocd typed ad-hoc instead of `fbuild
deploy <board>` skips CMSIS-DAP / SWD reset dispatch (#687), post-flash
VCOM unwedge timing (#565), artifact size validation (#690), and the
per-board handoff timing (#691). The result is a wedged VCOM on
Windows, silent flash failures past the flash region boundary, and
race conditions on monitor open. Policy through docs alone doesn't
hold — agents drift. Policy through hooks does.

## What this blocks

`pyocd`, `esptool` / `esptool.py`, `dfu-util`, `picotool`, `probe-rs`
when invoked as the bare command in a Bash invocation. False-positive
hardening — see `is_benign_mention` — prevents docs / commit-body
mentions from being flagged.

## Override

`FL_AGENT_ALLOW_ALL_CMDS=1` in the environment lets the call through.
Use only for legitimate debug-the-hook cases. Same env var name as
FastLED for muscle memory.

## Exit codes

  0 — allow (no output, or emit advisory JSON for the harness)
  2 — deny (stderr message routed back to Claude)
"""

import json
import os
import re
import sys


# Each forbidden command is a regex against the START of the Bash
# command string (after stripping leading whitespace + simple `env
# VAR=val` prefixes). Patterns must be standalone-word matches so
# `mypyocd-wrapper` or `not-esptool-py` don't false-fire.
FORBIDDEN_COMMANDS: dict[str, str] = {
    "pyocd": (
        "use `fbuild deploy <board>` or `bash autoresearch <board>` — the "
        "orchestrator dispatches CMSIS-DAP/SWD reset (fbuild#687), handles "
        "post-flash VCOM unwedge timing (fbuild#565), validates artifact "
        "size against flash region (fbuild#690), and times the handoff "
        "(fbuild#691). None of that runs when pyocd is invoked directly."
    ),
    "esptool": (
        "use `fbuild deploy -e <env>` instead — the deploy wrapper handles "
        "ROM-download-mode detection, USB CDC retry policy, and post-flash "
        "reset. Direct esptool calls skip all of it."
    ),
    "esptool.py": (
        "use `fbuild deploy -e <env>` instead — same reason as bare esptool."
    ),
    "dfu-util": (
        "use `fbuild deploy -e <env>` once the SAMD/STM32 deploy path lands. "
        "Direct dfu-util skips artifact-size validation (fbuild#690)."
    ),
    "picotool": (
        "use `fbuild deploy -e <env>` for RP2040. Direct picotool skips "
        "BOOTSEL re-enumeration timing (fbuild#691) and artifact size "
        "validation (fbuild#690)."
    ),
    "probe-rs": (
        "use `fbuild deploy -e <env>`. Direct probe-rs skips the same set "
        "of orchestration steps (fbuild#687/#690/#691). The probe-rs library "
        "is fine; the standalone CLI is what's banned."
    ),
}

OVERRIDE_ENV = "FL_AGENT_ALLOW_ALL_CMDS"


def _strip_prefixes(segment: str) -> str:
    s = re.sub(r"^env(?:\s+[A-Z_][A-Z0-9_]*=\S+)+\s+", "", segment.lstrip())
    s = re.sub(r"^sudo\s+", "", s)
    return s


def is_benign_mention(command: str, tool: str) -> bool:
    """Filter out string mentions that aren't actual invocations.

    Cases that must NOT trip the ban:

    - `git commit -m "fix: avoid pyocd race ..."` — commit body
    - `grep -r 'pyocd' docs/` — searching for the string
    - `echo "see esptool docs"` — echoing the name
    - `cat README.md | grep esptool` — reading a doc; `esptool` is an
      argument to grep, not a process name

    Heuristic: the tool is an invocation iff it's the first token of
    some pipe / `&&` / `||` / `;` segment of the command (after
    stripping `env VAR=val …` and `sudo` prefixes). Otherwise it's a
    mention.

    Lesson from FastLED's first revision of the hook (see #3339 review
    thread): the first cut over-fired on commit messages.
    """
    # Split on common shell separators that introduce a new subcommand.
    # This isn't a real shell parse — it's enough for the "tool is the
    # leading word of some segment" check we need.
    segments = re.split(r"\|{1,2}|&&|;", command)
    for segment in segments:
        stripped = _strip_prefixes(segment)
        if not stripped:
            continue
        first_token = stripped.split(None, 1)[0]
        if first_token == tool:
            return False
    return True


def find_forbidden(command: str) -> tuple[str, str] | None:
    """Return `(tool, advice)` if `command` invokes a forbidden tool.

    Iterates banned tools by descending name length so longer names
    (`esptool.py`) match before their prefixes (`esptool`).
    """
    for tool, advice in sorted(
        FORBIDDEN_COMMANDS.items(), key=lambda kv: -len(kv[0])
    ):
        if not re.search(rf"(?<![A-Za-z0-9_.-]){re.escape(tool)}(?![A-Za-z0-9_])", command):
            continue
        if is_benign_mention(command, tool):
            continue
        return tool, advice
    return None


def main() -> int:
    if os.environ.get(OVERRIDE_ENV) == "1":
        # Caller has explicitly opted into the ban override (debug-
        # the-hook etc.). Allow silently.
        return 0
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        return 0
    command = data.get("tool_input", {}).get("command", "")
    if not command:
        return 0
    hit = find_forbidden(command)
    if hit is None:
        return 0
    tool, advice = hit
    print(
        f"forbidden: `{tool}` — {advice}\n"
        f"override with `{OVERRIDE_ENV}=1` for legitimate debug-the-hook cases.",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    sys.exit(main())

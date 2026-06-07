"""Shared output-truncation helper for hooks.

Hooks pipe subprocess output back to Claude via stderr. When the underlying
tool (cargo clippy, cargo test, cc-rs build scripts, etc.) fails and dumps
megabytes of output — for example, cc-rs printing the full `PATH` env var
once per compiled C file, or Windows MAX_PATH C1081 errors from
deeply-nested worktree target dirs — the unbounded stream blows past
Claude's context window.

`truncate_output()` keeps the last `max_lines` of each stream and prefixes
a one-line "[... N earlier lines truncated ...]" header so the model can
see the tail (which is where the actionable error usually lives) without
ingesting the build-script preamble.
"""

from __future__ import annotations

DEFAULT_MAX_LINES = 200


def truncate_output(text: str, max_lines: int = DEFAULT_MAX_LINES) -> str:
    """Return `text` with at most `max_lines` trailing lines.

    If truncation occurs, a header line indicates how many earlier lines
    were dropped so the reader knows context is missing. Trailing/leading
    whitespace on the original text is preserved on the kept tail.
    """
    if max_lines <= 0:
        return text
    lines = text.splitlines()
    if len(lines) <= max_lines:
        return text
    skipped = len(lines) - max_lines
    kept = "\n".join(lines[-max_lines:])
    return f"[... {skipped} earlier line(s) truncated to fit context ...]\n{kept}"

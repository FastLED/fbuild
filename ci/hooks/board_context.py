#!/usr/bin/env python3
"""UserPromptSubmit hook: inject board-support skill guidance when the prompt
mentions boards, hardware, or board-related errors.

Reads the user's prompt from stdin (JSON with a "prompt" field) and outputs
additionalContext JSON when board-related keywords are detected.

Exit codes:
  0 - Always succeeds (either injects context or passes through silently)
"""

from __future__ import annotations

import json
import re
import sys


# Keywords that strongly suggest a board-related issue
BOARD_KEYWORDS = [
    r"\bboard\b",
    r"\bboards\.txt\b",
    r"\bboards\.json\b",
    r"\bBoardConfig\b",
    r"\bboard_id\b",
    r"\bboard.definition\b",
    r"\bmanifest\.json\b",
    r"\benrich.boards\b",
    r"\bvalidate.boards\b",
    r"\bboard.sources\b",
]

# MCU/platform names that suggest board work
PLATFORM_KEYWORDS = [
    r"\besp32\b",
    r"\besp8266\b",
    r"\batmega\b",
    r"\battiny\b",
    r"\bstm32\b",
    r"\brp2040\b",
    r"\brp2350\b",
    r"\bteensy\b",
    r"\bsamd\b",
    r"\bnrf52\b",
    r"\bavr\b",
]

# Error patterns that indicate board lookup failures
ERROR_KEYWORDS = [
    r"unknown board",
    r"board not found",
    r"no board",
    r"missing board",
    r"unsupported board",
    r"board.*missing",
    r"board.*wrong",
    r"board.*invalid",
    r"build.*flags.*wrong",
    r"variant.*not found",
    r"core.*not found",
]

CONTEXT_TEMPLATE = """\
## Board Support Context (auto-injected)

The user's prompt appears to involve board definitions or hardware support.
Before proceeding, consider using the **/board-support** skill which provides
step-by-step guidance for diagnosing board issues.

### Quick reference:
- **fbuild board database**: `crates/fbuild-config/assets/boards/json/{{board_id}}.json`
- **Board manifest**: `crates/fbuild-config/assets/boards/manifest.json`
- **Board config code**: `crates/fbuild-config/src/board.rs`
- **Validate against PlatformIO**: `uv run python ci/validate_boards.py`
- **Search external sources**: `uv run python ci/board_sources.py --search QUERY`
- **Compare coverage**: `uv run python ci/board_sources.py --compare`
- **Enrich from PlatformIO**: `soldr cargo run -p fbuild-config --bin enrich_boards`

### External board registries (for boards not in PlatformIO):
- Arduino package indices: `uv run python ci/board_sources.py --list-arduino`
- Zephyr boards: `uv run python ci/board_sources.py --list-zephyr`

### Workflow:
1. Check if board exists in fbuild (`manifest.json`)
2. If missing, search external sources (`board_sources.py --search`)
3. If found externally, create the board JSON and run enrichment
4. Validate: `uv run python ci/validate_boards.py`
"""


def detect_board_context(prompt: str) -> bool:
    """Return True if the prompt appears board-related."""
    lower = prompt.lower()

    for pattern in BOARD_KEYWORDS:
        if re.search(pattern, lower):
            return True

    for pattern in ERROR_KEYWORDS:
        if re.search(pattern, lower):
            return True

    # Platform keywords only trigger if combined with action words
    action_words = r"(add|fix|debug|configure|support|missing|wrong|error|issue|problem|create|update)"
    for pattern in PLATFORM_KEYWORDS:
        if re.search(pattern, lower) and re.search(action_words, lower):
            return True

    return False


def main() -> int:
    try:
        raw = sys.stdin.read()
        if not raw.strip():
            return 0
        data = json.loads(raw)
    except (json.JSONDecodeError, OSError):
        return 0

    prompt = data.get("prompt", "")
    if not prompt or not detect_board_context(prompt):
        return 0

    # Inject context
    output = {"additionalContext": CONTEXT_TEMPLATE}
    print(json.dumps(output))
    return 0


if __name__ == "__main__":
    sys.exit(main())

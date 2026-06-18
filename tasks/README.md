# Tasks

Historical planning notes for the fbuild Rust port. Active planning and tracking live in GitHub issues for `FastLED/fbuild`.

## Contents

- **`todo.md`** -- Platform-by-platform migration checklist with completed and pending items
- **`lessons.md`** -- Lessons learned from development (toolchain conflicts, clippy patterns, etc.)
- **`baseline-205.md`** -- Baseline ELF / TU-count measurements captured at the foundation-landed SHA for #205 (regenerate via `uv run python ci/measure_baseline_205.py`)
- **`zccache-kv-design.md`** -- Design note for the namespaced K/V store added to zccache (filed as `zackees/zccache#130`); prerequisite for #205 Phase 4 memoization.

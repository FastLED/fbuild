# Bench results

Raw timings + writeup from the `uv run` rebuild-speedup investigation
that ships with this directory.

- **`REPORT.md`** — analysis: what the bottleneck was, what was applied,
  before/after numbers, what's left.
- **`baseline.json`** — `ci/bench_uv_run.py` output against `main` before
  the fixes.
- **`after_fixes.json`** — same script after applying `CARGO_TARGET_DIR`
  pinning + `BuildWithCargo` mtime-skip + `no-build-isolation-package`
  + `cache-keys`.

Reproduce with `python ci/bench_uv_run.py <label>` (writes
`ci/bench-results/<label>.json`).

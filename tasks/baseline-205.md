# Baseline measurements for #205 — DEFERRED

Captured: 2026-04-24
Git SHA: (this PR's foundation commit — see PR description)
Branch: main
Tooling: `uv run python ci/measure_baseline_205.py`

## Status

The capture script (`ci/measure_baseline_205.py`) is implemented and
runnable. The actual data capture is **deferred to a follow-up step**
because Teensy/STM32 builds against the foundation-landed resolver are
heavyweight (multi-minute per board on a cold cache) and the build
infrastructure on the development workstation could not complete all
four boards within the agent run-window. Running the script in a clean
CI environment with all toolchains pre-warmed will populate the table
below.

## How to run

```bash
uv run python ci/measure_baseline_205.py --out tasks/baseline-205.md
uv run python ci/measure_baseline_205.py --targets teensyLC teensy41
```

The script:

1. Builds `tests/platform/<board>` for `teensyLC`, `teensy30`,
   `teensy41`, `stm32f103c8` via the existing `fbuild` CLI.
2. Counts distinct `file` entries in the resulting
   `compile_commands.json` (TU count).
3. Probes `firmware.elf` section sizes (`.text`, `.data`, `.bss`,
   `.dmabuffers`) via `arm-none-eabi-size` (preferred) or `llvm-size`.
4. Scans `compile_commands.json` for `FNET` / `Snooze` / `RadioHead`
   / `mbedtls` references — the four libraries that were wrongly
   selected before the foundation phases of #205 landed.

## Expected once captured

| env          | TU count | .text | .data | .bss | .dmabuffers | excluded libs |
|--------------|----------|-------|-------|------|-------------|----------------|
| teensyLC     | (≤ 250 per AC#1) | … | … | (≤ 3 KB per AC#1) | — | none of FNET/Snooze/RadioHead/mbedtls present |
| teensy30     | … | … | … | … | (≤ 1 KB per AC#2) | none |
| teensy41     | … | … | … | … | … | (regression baseline) |
| stm32f103c8  | … | … | … | … | — | (must include SPI per AC#4) |

## Why not just ship the placeholder and call it done

Phase 6 (acceptance gates) needs *real* numbers to anchor the
"+1%" / "≤ 250" / "≤ 3 KB" thresholds in the issue body. A guess will
be argued about during Phase 6 reviews. The capture has to happen on a
host that can actually link these four ELFs, which means either (a) a
clean CI runner with the Teensy/STM32 toolchains pinned, or (b) a
warmed local install where every framework download has already
completed. Neither was ready inside this PR's window.

## Tracking

This file is replaced wholesale on the next successful run of
`measure_baseline_205.py`. The non-empty rows above will be filled in
with measured numbers + an ISO timestamp + the exact git SHA the
measurement was taken against.

## Run command

```bash
uv run python ci/measure_baseline_205.py --out tasks/baseline-205.md
```

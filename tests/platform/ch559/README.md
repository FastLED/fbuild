## CH559 (WCH 8051) Smoke Fixture

Minimal CH559 / `intel_mcs51` project used to track fbuild support state
for the WCH CH55x family from FastLED/fbuild#384 (source issue:
FastLED/FastLED#1432).

### Current fbuild status (2026-06-03)

- Board metadata: present (`crates/fbuild-config/assets/boards/json/CH559.json`,
  along with ~250 other 8051 boards in the same directory).
- Platform string `intel_mcs51`: NOT mapped in `Platform::from_platform_str`
  (`crates/fbuild-core/src/lib.rs`), so any build/deploy/install_deps request
  for this fixture fails at the daemon boundary with
  `unsupported platform: intel_mcs51`
  (see `crates/fbuild-daemon/src/handlers/operations/build.rs`).
- `BuildOrchestrator` for 8051/SDCC: not implemented in `fbuild-build`. Even
  with a `Platform::IntelMcs51` variant, `get_platform_support` would return
  "native orchestrator for ... not yet implemented"
  (`crates/fbuild-build/src/lib.rs`).
- FastLED platform header for 8051: missing. `src/led_sysdefs.h` has no
  `__SDCC__` / `__SDCC_mcs51` / `defined(__C51__)` branch, so even with a
  working backend FastLED itself bails out with
  `#error "This platform isn't recognized by FastLED... yet."`.

This fixture intentionally does NOT `#include <FastLED.h>` — the FastLED-side
blocker would mask any backend progress. It exists so that, once both an
8051 orchestrator lands in fbuild and FastLED ships a CH55x platform header,
this directory can be flipped to a real FastLED smoke build with a single edit.

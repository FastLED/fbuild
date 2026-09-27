# ATtiny13 (MicroCore) Tracker Fixture

Minimal ATtiny13 project used to track fbuild support state for the smallest
classic ATtiny class. Source FastLED issue: FastLED/FastLED#581. fbuild
tracker: FastLED/fbuild#389.

## Current fbuild status (2026-09-27)

- **Board metadata**: present
  (`crates/fbuild-config/assets/boards/json/attiny13.json`,
  `attiny13a.json`). Both declare `core = MicroCore`.
- **Framework registry**: `MicroCore` is mapped to a checksummed PlatformIO
  archive in `crates/fbuild-library/assets/avr_frameworks.json`. Resolving the
  framework is no longer the expected blocker; an end-to-end tiny-chip build
  remains a separate validation step.
- **FastLED pin map**: present
  (`src/platforms/avr/attiny/pins/fastpin_attiny.h` has an
  `__AVR_ATtiny13__` branch).

## What this fixture proves

This fixture intentionally does NOT `#include <FastLED.h>` so it can first
validate MicroCore and toolchain integration without a separate FastLED
compatibility or flash-size failure masking it.

## Why a 1 KiB flash chip matters

The ATtiny13 has only 1 KiB of flash and 64 B of RAM. A realistic FastLED
demo will not fit; the value of this fixture is verifying that the
framework + toolchain wiring resolves at all, not that a useful sketch
links.

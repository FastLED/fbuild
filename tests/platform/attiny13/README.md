# ATtiny13 (MicroCore) Tracker Fixture

Minimal ATtiny13 project used to track fbuild support state for the smallest
classic ATtiny class. Source FastLED issue: FastLED/FastLED#581. fbuild
tracker: FastLED/fbuild#389.

## Current fbuild status (2026-06-03)

- **Board metadata**: present
  (`crates/fbuild-config/assets/boards/json/attiny13.json`,
  `attiny13a.json`). Both declare `core = MicroCore`.
- **Framework registry**: `MicroCore` is NOT mapped in
  `crates/fbuild-packages/assets/avr_frameworks.json`. The AVR
  orchestrator will fail at `AvrFramework::for_core("MicroCore", ..)`
  with "no AVR framework registered for core 'MicroCore'".
- **FastLED pin map**: present
  (`src/platforms/avr/attiny/pins/fastpin_attiny.h` has an
  `__AVR_ATtiny13__` branch).

## What this fixture proves

This fixture intentionally does NOT `#include <FastLED.h>` — the fbuild
framework-resolve step fails first, so adding FastLED would only mask the
real blocker. Once `MicroCore` is added to `avr_frameworks.json` (github =
`MCUdude/MicroCore`, validation_path = `cores/MicroCore/Arduino.h`), this
directory can be flipped to a real FastLED smoke build with a single edit.

## Why a 1 KiB flash chip matters

The ATtiny13 has only 1 KiB of flash and 64 B of RAM. A realistic FastLED
demo will not fit; the value of this fixture is verifying that the
framework + toolchain wiring resolves at all, not that a useful sketch
links.

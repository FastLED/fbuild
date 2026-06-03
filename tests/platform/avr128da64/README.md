# AVR128DA64 (DxCore) Tracker Fixture

Minimal AVR128DA64 project used to track fbuild support state for the
AVR-Dx (DA / DB / DD) family. Source FastLED issue:
FastLED/FastLED#1307. fbuild tracker: FastLED/fbuild#389.

## Current fbuild status (2026-06-03)

- **Board metadata**: present for the full AVR-Dx matrix
  (`crates/fbuild-config/assets/boards/json/AVR128DA*.json`,
  `AVR128DB*.json`, `AVR64DA*.json`, `AVR64DB*.json`, `AVR64DD*.json`,
  `AVR32DA*.json`, `AVR32DB*.json`, `AVR64DD14/20/28/32.json`).
  All entries declare `platform = atmelmegaavr` and `core = dxcore`.
- **Framework registry**: `dxcore` is NOT mapped in
  `crates/fbuild-packages/assets/avr_frameworks.json`. The AVR
  orchestrator will fail at `AvrFramework::for_core("dxcore", ..)`
  with "no AVR framework registered for core 'dxcore'".
- **FastLED platform support**: NO `AVR_DA` / `AVR128DA*` /
  `__AVR_AVR128DA*__` branch in `src/platforms/avr/`. The user-supplied
  pin map in FastLED/FastLED#1307 has not been merged into
  `src/platforms/avr/atmega/` or similar.

## What this fixture proves

This fixture intentionally does NOT `#include <FastLED.h>` — fbuild fails
at framework resolve first, and even if it didn't, FastLED would `#error`
on the missing platform support. Both gaps must close before this
directory can be flipped to a real FastLED smoke build.

## Next steps (fbuild-side)

Add a `dxcore` entry to `avr_frameworks.json` (github =
`SpenceKonde/DxCore`, validation_path = `cores/dxcore/Arduino.h`,
core_dir = `dxcore`). Possibly also gate the megaTinyCore-style toolchain
on the newer AVR-LibC / GCC bundle that DxCore ships with.

## Next steps (FastLED-side)

Land an AVR-Dx pin map alongside `src/platforms/avr/atmega/m4809/` so
that `__AVR_AVR128DA64__` / `__AVR_AVR128DB64__` / etc. resolve to
`_FL_DEFPIN(...)` macros consistent with DxCore's port mapping. The pin
table in FastLED/FastLED#1307 is a starting point. Hardware SPI defs
need `SPI_DATA` / `SPI_CLOCK` constants for the AVR-Dx SPI peripheral
(USART-based fallback is also an option).

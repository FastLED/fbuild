# AVR128DA64 (DxCore) Tracker Fixture

Minimal AVR128DA64 project used to track fbuild support state for the
AVR-Dx (DA / DB / DD) family. Source FastLED issue:
FastLED/FastLED#1307. fbuild tracker: FastLED/fbuild#389.

## Current fbuild status (2026-09-27)

- **Board metadata**: present for the full AVR-Dx matrix
  (`crates/fbuild-config/assets/boards/json/AVR128DA*.json`,
  `AVR128DB*.json`, `AVR64DA*.json`, `AVR64DB*.json`, `AVR64DD*.json`,
  `AVR32DA*.json`, `AVR32DB*.json`, `AVR64DD14/20/28/32.json`).
  All entries declare `platform = atmelmegaavr` and `core = dxcore`.
- **Framework registry**: `dxcore` is mapped to a checksummed PlatformIO
  archive in `crates/fbuild-library/assets/avr_frameworks.json`. The
  megaAVR adapter selects PlatformIO's newer AVR-GCC requirement for DxCore
  when the platform version is pinned. The `avr128da64_registry` environment
  completed a Linux smoke build; other hosts remain to be validated by CI.
- **FastLED platform support**: NO `AVR_DA` / `AVR128DA*` /
  `__AVR_AVR128DA*__` branch in `src/platforms/avr/`. The user-supplied
  pin map in FastLED/FastLED#1307 has not been merged into
  `src/platforms/avr/atmega/` or similar.

## What this fixture proves

This fixture intentionally does NOT `#include <FastLED.h>` so it can validate
the fbuild-side framework/toolchain path. FastLED's AVR-Dx support remains
separate and may still reject a FastLED sketch.

`avr128da64_registry` pins megaAVR platform 1.10.0 and DxCore 1.6.2, which
selects the platform builder's newer AVR-GCC package.

## Next steps (fbuild-side)

Run the fixture as a real build on the remaining supported hosts and fix any
host-specific compiler/linker incompatibility independently of package
resolution.

## Next steps (FastLED-side)

Land an AVR-Dx pin map alongside `src/platforms/avr/atmega/m4809/` so
that `__AVR_AVR128DA64__` / `__AVR_AVR128DB64__` / etc. resolve to
`_FL_DEFPIN(...)` macros consistent with DxCore's port mapping. The pin
table in FastLED/FastLED#1307 is a starting point. Hardware SPI defs
need `SPI_DATA` / `SPI_CLOCK` constants for the AVR-Dx SPI peripheral
(USART-based fallback is also an option).

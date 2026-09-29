# ATmega1284P Tracker Fixture

Minimal ATmega1284P project (Microduino Core+ variant) used to track
fbuild support for the ATmega644/1284 class. Source FastLED issue:
FastLED/FastLED#1253. fbuild tracker: FastLED/fbuild#389.

## Current fbuild status (2026-06-03)

- **Board metadata**: present
  (`crates/fbuild-config/assets/boards/json/1284p16m.json`,
  `ATmega1284P.json`, `sanguino_atmega1284p.json`, `sanguino_atmega1284_8m.json`).
- **Framework registry**: `MiniCore` IS mapped in
  `crates/fbuild-packages/assets/avr_frameworks.json` (MCUdude/MiniCore
  v2.2.2, `core_dir = MCUdude_corefiles`). The `1284p16m` JSON however
  declares `core = arduino` and `variant = microduino_plus`, so the
  build resolves through ArduinoCore-avr, not MiniCore.
- **FastLED platform support**: ATmega1284 / ATmega1284P present in
  `src/platforms/avr/atmega/common/fastpin_legacy_other.h` and
  `fastpin_avr_legacy_dispatcher.h`.

## Source-issue takeaway

FastLED/FastLED#1253 is asking for alternative pin mappings (Bobuino
pinout) on the ATmega1284. That's a FastLED-side change: the FastPin
variant selector would need to look at the variant-specific macro the
core defines and choose a different pin map. There's no fbuild-side
blocker for the default ATmega1284P pin layout, so this fixture is
intentionally minimal — it just proves the toolchain + framework wiring
is healthy.

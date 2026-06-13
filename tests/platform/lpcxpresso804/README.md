# LPCXpresso804 build fixture

Arduino-framework build fixture for the NXP **LPCXpresso804** (LPC804M101JDH24,
Cortex-M0+, 32 KB Flash, 4 KB RAM, 15 MHz).

Drives `.github/workflows/build-lpcxpresso804.yml` via `template_build.yml`.
The `NxpLpc` orchestrator compiles this empty sketch against the
`zackees/ArduinoCore-LPC8xx` Arduino core (`framework = arduino`,
`board = lpcxpresso804`) and links `firmware.bin`.

Part of Stage 8 of META FastLED/fbuild#487 (#528).

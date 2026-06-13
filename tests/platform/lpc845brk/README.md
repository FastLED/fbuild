# LPC845-BRK build fixture

Arduino-framework build fixture for the NXP **LPC845-BRK** (LPC845M301JBD48,
Cortex-M0+, 64 KB Flash, 16 KB RAM, 30 MHz).

Drives `.github/workflows/build-lpc845brk.yml` via `template_build.yml`.
The `NxpLpc` orchestrator compiles this empty sketch against the
`zackees/ArduinoCore-LPC8xx` Arduino core (`framework = arduino`,
`board = lpc845brk`) and links `firmware.bin`.

Part of Stage 8 of META FastLED/fbuild#487 (#528).

# Adafruit Matrix Portal M4 Test Fixture

ARM Cortex-M4F build validation project for the Adafruit Matrix Portal M4 (SAMD51J19A, the same MCU as the Feather M4 but with the `matrixportal_m4` variant and `-DARDUINO_MATRIXPORTAL_M4` / `-DADAFRUIT_MATRIXPORTAL_M4_EXPRESS` defines). Uses the Arduino framework on the `atmelsam` platform.

Tracks FastLED issue #1196 — Arduino IDE build failures on this board. fbuild's role here is to prove the board's toolchain + Arduino core + variant compile cleanly; any remaining failures are FastLED-side example/pin-map issues.

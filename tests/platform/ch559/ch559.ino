// Minimal CH559 (WCH 8051) sketch used as the fbuild smoke fixture for
// FastLED/fbuild#384. Kept as a bare setup()/loop() instead of pulling in
// FastLED because FastLED's `led_sysdefs.h` has no 8051/SDCC branch yet, so
// any `#include <FastLED.h>` would terminate at the platform-not-recognized
// `#error` regardless of toolchain state.

void setup() {}

void loop() {}

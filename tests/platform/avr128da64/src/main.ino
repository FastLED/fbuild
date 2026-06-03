// Minimal AVR128DA64 sketch used as the fbuild smoke fixture for
// FastLED/fbuild#389 (source: FastLED/FastLED#1307 "AVR128DA64").
//
// Kept as a bare setup()/loop() rather than including FastLED because
// (a) the DxCore framework is not yet wired into
// `crates/fbuild-packages/assets/avr_frameworks.json`, and (b) FastLED's
// `src/platforms/avr/` has no AVR-Dx pin support / hardware SPI defs.
// Either gap would terminate the build before the AVR-Dx-specific code
// path runs.
#include <Arduino.h>

void setup() {
    pinMode(LED_BUILTIN, OUTPUT);
}

void loop() {
    digitalWrite(LED_BUILTIN, HIGH);
    delay(500);
    digitalWrite(LED_BUILTIN, LOW);
    delay(500);
}

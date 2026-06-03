// Minimal ATtiny13 sketch used as the fbuild smoke fixture for
// FastLED/fbuild#389 (source: FastLED/FastLED#581 "Need ATTiny13 support").
//
// Kept as a bare setup()/loop() rather than including FastLED because
// (a) the MicroCore framework is not yet wired into
// `crates/fbuild-packages/assets/avr_frameworks.json` so the build will
// stop at the framework-resolve step regardless of FastLED inclusion,
// and (b) the ATtiny13 has only 1 KiB of flash / 64 B of RAM, so the
// representative FastLED smoke is "platform compiles" rather than a
// real LED-driving program.
#include <Arduino.h>

void setup() {
    pinMode(1, OUTPUT);
}

void loop() {
    digitalWrite(1, HIGH);
    delay(500);
    digitalWrite(1, LOW);
    delay(500);
}

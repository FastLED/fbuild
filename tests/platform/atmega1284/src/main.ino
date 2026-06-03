// Minimal ATmega1284P (Microduino Core+ variant) sketch used as the fbuild
// smoke fixture for FastLED/fbuild#389 (source: FastLED/FastLED#1253
// "ATmega1284 alternative pin mappings").
//
// Kept as a bare setup()/loop() so the fbuild side proves out independently
// of the FastLED-side Bobuino-pinout discussion in the source issue.
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

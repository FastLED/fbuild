// LPC845 Arduino-framework Blink fixture (FastLED/fbuild#479).
//
// Exercises the minimum Arduino surface the orchestrator must compile and
// link: pinMode + digitalWrite (GPIO) and delay (SysTick timing). The
// bundled nxplpc Arduino core resolves these symbols; a project that ships
// its own variant core (zackees/ArduinoCore-LPC8xx) provides the real
// register-level GPIO implementation.

#include <Arduino.h>

#ifndef LED_BUILTIN
#define LED_BUILTIN 0
#endif

void setup() {
    pinMode(LED_BUILTIN, OUTPUT);
}

void loop() {
    digitalWrite(LED_BUILTIN, HIGH);
    delay(500);
    digitalWrite(LED_BUILTIN, LOW);
    delay(500);
}

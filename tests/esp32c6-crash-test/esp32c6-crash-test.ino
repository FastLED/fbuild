// Test sketch that deliberately crashes to validate crash decoding.
// After printing a marker string, calls abort() so the serial monitor
// can verify that the crash dump is decoded into symbolic function names.

#include <Arduino.h>
#include <cstdlib>

#define BAUD_RATE 115200

// Marker printed before the crash so test harness knows the sketch ran.
static const char* CRASH_MARKER = "FBUILD_CRASH_DECODE_TEST_START";

// Put the crash in a named function so addr2line output is easy to verify.
void __attribute__((noinline)) deliberate_crash() {
    Serial.println("About to call abort()...");
    Serial.flush();
    abort();
}

void __attribute__((noinline)) crash_wrapper() {
    deliberate_crash();
}

void setup() {
    Serial.begin(BAUD_RATE);
    delay(3000);  // Wait for serial monitor to attach

    Serial.println(CRASH_MARKER);
    Serial.println("ESP32-C6 Crash Decode Test");
    Serial.flush();

    // Trigger crash from a named call chain:
    //   setup -> crash_wrapper -> deliberate_crash -> abort
    crash_wrapper();
}

void loop() {
    // Never reached — device crashes in setup().
}

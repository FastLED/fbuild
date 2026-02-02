// Simple Blink example using FastLED
// This should NOT include any stb_vorbis functionality
// Tests LTO dead code elimination

#include <FastLED.h>

#define NUM_LEDS 1
#define DATA_PIN 6

CRGB leds[NUM_LEDS];

void setup() {
    FastLED.addLeds<WS2812B, DATA_PIN, GRB>(leds, NUM_LEDS);
}

void loop() {
    leds[0] = CRGB::Red;
    FastLED.show();
    delay(500);

    leds[0] = CRGB::Black;
    FastLED.show();
    delay(500);
}

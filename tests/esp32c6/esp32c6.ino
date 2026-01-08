#include <FastLED.h>

#define NUM_LEDS 10
#define DATA_PIN 6

CRGB leds[NUM_LEDS];

void setup() {
  Serial.begin(115200);
  pinMode(LED_BUILTIN, OUTPUT);
  FastLED.addLeds<WS2812, DATA_PIN, GRB>(leds, NUM_LEDS);

  delay(1000);  // wait for host to connect
  Serial.println("TEST PASSED");
}

void loop() {
  digitalWrite(LED_BUILTIN, HIGH);

  // Cycle through colors
  for(int i = 0; i < NUM_LEDS; i++) {
    leds[i] = CRGB::Red;
  }
  FastLED.show();
  delay(500);

  for(int i = 0; i < NUM_LEDS; i++) {
    leds[i] = CRGB::Green;
  }
  FastLED.show();
  delay(500);

  digitalWrite(LED_BUILTIN, LOW);

  for(int i = 0; i < NUM_LEDS; i++) {
    leds[i] = CRGB::Blue;
  }
  FastLED.show();
  delay(500);
}


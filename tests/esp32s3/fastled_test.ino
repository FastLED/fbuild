#include <FastLED.h>

// GPIO pin definitions for ESP32-S3
#define LED_PIN 48      // Built-in LED on ESP32-S3-DevKitC-1
#define NUM_LEDS 1      // Single LED for testing
#define BAUD_RATE 115200

// LED array
CRGB leds[NUM_LEDS];

void setup() {
  // Initialize serial communication
  Serial.begin(BAUD_RATE);

  // Wait for serial connection (or timeout after 5 seconds)
  unsigned long startTime = millis();
  while (!Serial && (millis() - startTime < 5000)) {
    delay(10);
  }

  Serial.println("FBUILD_FASTLED_TEST_SETUP_START");

  // Initialize FastLED
  FastLED.addLeds<WS2812B, LED_PIN, GRB>(leds, NUM_LEDS);
  FastLED.setBrightness(50);

  Serial.println("FBUILD_FASTLED_TEST_SETUP_COMPLETE");
  Serial.println("ESP32-S3 FastLED Test Initialized");
}

void loop() {
  static int hue = 0;
  static int loopCount = 0;

  loopCount++;
  Serial.print("FBUILD_FASTLED_LOOP_");
  Serial.println(loopCount);

  // Cycle through rainbow colors
  leds[0] = CHSV(hue, 255, 255);
  FastLED.show();

  hue = (hue + 1) % 256;
  delay(20);
}

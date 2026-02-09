#include <Arduino.h>
// This include triggers the bug - esp_bt.h has broken includes
#include "esp_bt.h"

void setup() {
  // Minimal setup - we just need to trigger compilation
  Serial.begin(115200);
}

void loop() {
  // Empty loop
  delay(1000);
}

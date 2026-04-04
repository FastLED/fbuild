#include <Arduino.h>

void setup() {
    Serial.begin(115200);
    // GPIO 8 is commonly available on ESP32-C3 DevKit
    pinMode(8, OUTPUT);
}

void loop() {
    digitalWrite(8, HIGH);
    delay(500);
    digitalWrite(8, LOW);
    delay(500);
    Serial.println("Hello from ESP32-C3!");
}

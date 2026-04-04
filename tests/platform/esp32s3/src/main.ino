#include <Arduino.h>

void setup() {
    Serial.begin(115200);
    pinMode(2, OUTPUT);
}

void loop() {
    digitalWrite(2, HIGH);
    delay(500);
    digitalWrite(2, LOW);
    delay(500);
    Serial.println("Hello from ESP32-S3!");
}

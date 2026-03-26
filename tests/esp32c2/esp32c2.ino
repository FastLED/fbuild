// GPIO 8 is commonly used as LED on ESP32-C2 DevKit
#define LED_PIN 8
#define BAUD_RATE 115200

void setup() {
  Serial.begin(BAUD_RATE);
  pinMode(LED_PIN, OUTPUT);

  delay(1000);  // wait for host to connect
  Serial.println("TEST PASSED");
}

void loop() {
  digitalWrite(LED_PIN, HIGH);
  delay(500);
  digitalWrite(LED_PIN, LOW);
  delay(500);
}

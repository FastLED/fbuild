// GPIO 2 is commonly used as LED on ESP32
#define LED_PIN 2

void setup() {
  Serial.begin(115200);
  pinMode(LED_PIN, OUTPUT);

  // Wait for serial connection
  delay(1000);

  // Output the test pattern that we're looking for
  Serial.println("TEST PASSED/");
}

void loop() {
  digitalWrite(LED_PIN, HIGH);
  delay(1000);
  digitalWrite(LED_PIN, LOW);
  delay(1000);
}

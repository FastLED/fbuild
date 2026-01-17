/*
 * Raspberry Pi Pico (RP2040) Blink Test
 *
 * This sketch blinks the built-in LED on the Raspberry Pi Pico.
 * The built-in LED is connected to GPIO 25.
 */

#define LED_PIN 25

void setup() {
  // Initialize the LED pin as an output
  pinMode(LED_PIN, OUTPUT);

  // Initialize serial communication
  Serial.begin(115200);
  while (!Serial) {
    ; // Wait for serial port to connect (needed for native USB)
  }

  Serial.println("Raspberry Pi Pico Blink Test");
  Serial.println("RP2040 @ 133MHz");
}

void loop() {
  digitalWrite(LED_PIN, HIGH);
  Serial.println("LED ON");
  delay(1000);

  digitalWrite(LED_PIN, LOW);
  Serial.println("LED OFF");
  delay(1000);
}

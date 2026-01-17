/*
 * Raspberry Pi Pico 2 (RP2350) Blink Test
 *
 * This sketch blinks the built-in LED on the Raspberry Pi Pico 2.
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

  Serial.println("Raspberry Pi Pico 2 Blink Test");
  Serial.println("RP2350 @ 150MHz");
  Serial.println("Cortex-M33 with FPU and DSP");
}

void loop() {
  digitalWrite(LED_PIN, HIGH);
  Serial.println("LED ON");
  delay(500);  // Faster blink to show performance

  digitalWrite(LED_PIN, LOW);
  Serial.println("LED OFF");
  delay(500);
}

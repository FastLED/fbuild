/*
 * ESP8266 Blink Test
 * Blinks the built-in LED on NodeMCU/WeMos D1 Mini
 */

#define LED_BUILTIN 2  // GPIO2 on most ESP8266 boards

void setup() {
  Serial.begin(115200);
  pinMode(LED_BUILTIN, OUTPUT);
  Serial.println("\nESP8266 Blink Test Started");
}

void loop() {
  digitalWrite(LED_BUILTIN, LOW);   // LED on (active low)
  Serial.println("LED ON");
  delay(1000);

  digitalWrite(LED_BUILTIN, HIGH);  // LED off
  Serial.println("LED OFF");
  delay(1000);
}

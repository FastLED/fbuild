void setup() {
  Serial.begin(115200);
  pinMode(LED_BUILTIN, OUTPUT);

  delay(1000);  // wait for host to connect
  Serial.println("TEST PASSED/");
}

void loop() {
  digitalWrite(LED_BUILTIN, HIGH);
  delay(500);
  digitalWrite(LED_BUILTIN, LOW);
  delay(500);
}


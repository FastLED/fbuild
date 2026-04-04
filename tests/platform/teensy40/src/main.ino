void setup() {
    pinMode(LED_BUILTIN, OUTPUT);
}

void loop() {
    digitalWriteFast(LED_BUILTIN, HIGH);
    delay(500);
    digitalWriteFast(LED_BUILTIN, LOW);
    delay(500);
}

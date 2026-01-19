// GPIO 2 is commonly used as LED on ESP32
#define LED_PIN 2
#define BAUD_RATE 115200

// Counter for loop iterations
static int loop_count = 0;

void setup() {
  Serial.begin(BAUD_RATE);
  pinMode(LED_PIN, OUTPUT);

  // Wait for 5 seconds before printing setup message
  delay(5000);

  Serial.println("FBUILD_QEMU_SERIAL_TEST_SETUP_COMPLETE");
  Serial.println("ESP32 Blink Sketch Initialized");
}

void loop() {
  loop_count++;

  // Print loop message with counter
  Serial.print("FBUILD_QEMU_LOOP_ITERATION_");
  Serial.println(loop_count);

  digitalWrite(LED_PIN, HIGH);
  delay(1000);
  digitalWrite(LED_PIN, LOW);
  delay(1000);
}

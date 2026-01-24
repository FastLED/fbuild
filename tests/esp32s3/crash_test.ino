// Deliberately crashing firmware to test crash-loop recovery
// This will immediately crash on boot to create a rapid reboot cycle

void setup() {
  // Crash immediately by accessing invalid memory
  // This creates a rapid boot loop that's hard to recover from
  volatile int* bad_ptr = (volatile int*)0x00000000;
  *bad_ptr = 42;  // Immediate crash
}

void loop() {
  // Never reached
}

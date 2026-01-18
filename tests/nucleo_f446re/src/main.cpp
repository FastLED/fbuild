/**
 * STM32F446RE Blink Test
 *
 * Simple LED blink sketch for ST Nucleo F446RE board.
 * The built-in LED is connected to PA5 (Arduino pin 13).
 *
 * This test validates:
 * - Arduino core compilation for STM32F4
 * - GPIO output functionality
 * - Basic timing functions
 */

#include <Arduino.h>

// Built-in LED on Nucleo F446RE is on PA5 (D13)
#define LED_BUILTIN PA5

void setup() {
    // Initialize serial communication at 115200 baud
    Serial.begin(115200);
    while (!Serial) {
        ; // Wait for serial port to connect (needed for USB serial)
    }

    Serial.println("STM32F446RE Blink Test Starting...");
    Serial.print("CPU Frequency: ");
    Serial.print(F_CPU / 1000000);
    Serial.println(" MHz");

    // Initialize the LED pin as output
    pinMode(LED_BUILTIN, OUTPUT);

    Serial.println("Setup complete!");
}

void loop() {
    // Turn on the LED
    digitalWrite(LED_BUILTIN, HIGH);
    Serial.println("LED ON");
    delay(500);

    // Turn off the LED
    digitalWrite(LED_BUILTIN, LOW);
    Serial.println("LED OFF");
    delay(500);
}

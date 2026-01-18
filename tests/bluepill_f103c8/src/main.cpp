/**
 * STM32F103C8T6 BluePill Blink Test
 *
 * Simple LED blink sketch for the popular BluePill board.
 * The built-in LED is connected to PC13 (active LOW).
 *
 * This test validates:
 * - Arduino core compilation for STM32F1
 * - GPIO output functionality
 * - Basic timing functions
 */

#include <Arduino.h>

// Built-in LED on BluePill is on PC13 (active LOW)
#define LED_BUILTIN PC13

void setup() {
    // Initialize serial communication at 115200 baud
    Serial.begin(115200);

    Serial.println("STM32F103C8 BluePill Blink Test Starting...");
    Serial.print("CPU Frequency: ");
    Serial.print(F_CPU / 1000000);
    Serial.println(" MHz");

    // Initialize the LED pin as output
    pinMode(LED_BUILTIN, OUTPUT);

    Serial.println("Setup complete!");
}

void loop() {
    // Turn on the LED (active LOW, so LOW turns it on)
    digitalWrite(LED_BUILTIN, LOW);
    Serial.println("LED ON");
    delay(500);

    // Turn off the LED (active LOW, so HIGH turns it off)
    digitalWrite(LED_BUILTIN, HIGH);
    Serial.println("LED OFF");
    delay(500);
}

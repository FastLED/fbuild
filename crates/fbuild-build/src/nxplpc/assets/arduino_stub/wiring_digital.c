// SPDX-License-Identifier: BSD-3-Clause
#include "Arduino.h"

void pinMode(pin_size_t pin, PinMode mode) {
    (void)pin;
    (void)mode;
}

void digitalWrite(pin_size_t pin, PinStatus value) {
    (void)pin;
    (void)value;
}

PinStatus digitalRead(pin_size_t pin) {
    (void)pin;
    return LOW;
}

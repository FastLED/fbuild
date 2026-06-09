// SPDX-License-Identifier: BSD-3-Clause
// Minimal Arduino-compatible surface for fbuild's NXP LPC8xx target.
#pragma once

#include <stddef.h>
#include <stdint.h>
#ifndef __cplusplus
#include <stdbool.h>
#endif

// Stage-3 hookup (FastLED/fbuild#479, partial): if the project ships its own
// Arduino-style variant directory (e.g. zackees/ArduinoCore-LPC8xx with
// `<project>/variants/<variant>/pins_arduino.h`), fbuild's nxplpc
// orchestrator prepends that directory to the include path. When that's the
// case, the variant's `pins_arduino.h` (typically `#include "variant.h"` →
// `LED_BUILTIN`, `PIN_SPI_*`, etc.) becomes available here transparently.
// Without a project-local variant the include is a no-op and the bundled
// stub's behavior is unchanged.
#if defined(__has_include)
#  if __has_include("pins_arduino.h")
#    include "pins_arduino.h"
#  endif
#endif

#ifdef __cplusplus
extern "C" {
#endif

#define HIGH 0x1
#define LOW 0x0

#define INPUT 0x0
#define OUTPUT 0x1
#define INPUT_PULLUP 0x2

typedef uint8_t byte;
typedef bool boolean;
typedef uint8_t pin_size_t;
typedef uint8_t PinStatus;
typedef uint8_t PinMode;

void pinMode(pin_size_t pin, PinMode mode);
void digitalWrite(pin_size_t pin, PinStatus value);
PinStatus digitalRead(pin_size_t pin);

void delay(uint32_t ms);
void delayMicroseconds(uint32_t us);
uint32_t millis(void);
uint32_t micros(void);
void yield(void);

#ifdef __cplusplus
}

#define F(str) (str)

#include "HardwareSerial.h"
#include "SPI.h"

#endif

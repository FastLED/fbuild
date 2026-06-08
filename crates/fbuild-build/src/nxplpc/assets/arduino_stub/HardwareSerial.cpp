// SPDX-License-Identifier: BSD-3-Clause
#include "HardwareSerial.h"

HardwareSerial Serial;

void HardwareSerial::begin(uint32_t baud) {
    (void)baud;
}

void HardwareSerial::end() {}

int HardwareSerial::available() {
    return 0;
}

int HardwareSerial::read() {
    return -1;
}

int HardwareSerial::peek() {
    return -1;
}

void HardwareSerial::flush() {}

size_t HardwareSerial::write(uint8_t value) {
    (void)value;
    return 1;
}

size_t HardwareSerial::write(const uint8_t* buffer, size_t size) {
    (void)buffer;
    return size;
}

size_t HardwareSerial::print(const char* str) {
    size_t len = 0;
    while (str && str[len]) {
        ++len;
    }
    return write(reinterpret_cast<const uint8_t*>(str), len);
}

size_t HardwareSerial::print(int value) {
    (void)value;
    return 0;
}

size_t HardwareSerial::print(unsigned int value) {
    (void)value;
    return 0;
}

size_t HardwareSerial::print(long value) {
    (void)value;
    return 0;
}

size_t HardwareSerial::print(unsigned long value) {
    (void)value;
    return 0;
}

size_t HardwareSerial::println() {
    static const uint8_t newline[] = {'\r', '\n'};
    return write(newline, sizeof(newline));
}

size_t HardwareSerial::println(const char* str) {
    return print(str) + println();
}

size_t HardwareSerial::println(int value) {
    return print(value) + println();
}

size_t HardwareSerial::println(unsigned int value) {
    return print(value) + println();
}

size_t HardwareSerial::println(long value) {
    return print(value) + println();
}

size_t HardwareSerial::println(unsigned long value) {
    return print(value) + println();
}

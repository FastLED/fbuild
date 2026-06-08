// SPDX-License-Identifier: BSD-3-Clause
#pragma once

#include <stddef.h>
#include <stdint.h>

class HardwareSerial {
public:
    void begin(uint32_t baud);
    void end();
    int available();
    int read();
    int peek();
    void flush();
    size_t write(uint8_t value);
    size_t write(const uint8_t* buffer, size_t size);
    size_t print(const char* str);
    size_t print(int value);
    size_t print(unsigned int value);
    size_t print(long value);
    size_t print(unsigned long value);
    size_t println();
    size_t println(const char* str);
    size_t println(int value);
    size_t println(unsigned int value);
    size_t println(long value);
    size_t println(unsigned long value);
};

extern HardwareSerial Serial;

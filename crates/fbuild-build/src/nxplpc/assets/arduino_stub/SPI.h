// SPDX-License-Identifier: BSD-3-Clause
#pragma once

#include <stddef.h>
#include <stdint.h>

#define SPI_CLOCK_DIV2 2
#define SPI_CLOCK_DIV4 4
#define SPI_CLOCK_DIV8 8
#define SPI_CLOCK_DIV16 16

#define MSBFIRST 1
#define LSBFIRST 0

#define SPI_MODE0 0
#define SPI_MODE1 1
#define SPI_MODE2 2
#define SPI_MODE3 3

class SPIClass {
public:
    void begin();
    void end();
    uint8_t transfer(uint8_t value);
    void transfer(void* buffer, size_t size);
    void setClockDivider(uint8_t divider);
    void setBitOrder(uint8_t order);
    void setDataMode(uint8_t mode);
};

extern SPIClass SPI;

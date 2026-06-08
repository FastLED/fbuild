// SPDX-License-Identifier: BSD-3-Clause
#include "SPI.h"

SPIClass SPI;

void SPIClass::begin() {}

void SPIClass::end() {}

uint8_t SPIClass::transfer(uint8_t value) {
    return value;
}

void SPIClass::transfer(void* buffer, size_t size) {
    (void)buffer;
    (void)size;
}

void SPIClass::setClockDivider(uint8_t divider) {
    (void)divider;
}

void SPIClass::setBitOrder(uint8_t order) {
    (void)order;
}

void SPIClass::setDataMode(uint8_t mode) {
    (void)mode;
}

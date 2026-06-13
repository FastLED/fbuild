// SPDX-License-Identifier: BSD-3-Clause
//
// LPC845 USART0 implementation of the Arduino HardwareSerial API. The
// LPC845-BRK onboard VCOM bridge is wired to P0_25 (U0_TXD) / P0_24 (U0_RXD).
// SystemInit (startup_lpc845.S) brings the core to 24 MHz via FRO direct, which
// divides accurately to 115200 baud; the BRG below is computed from F_CPU so it
// stays correct if the board's f_cpu changes. Register map per UM11029.
#include "HardwareSerial.h"

#ifndef F_CPU
#define F_CPU 24000000UL
#endif

namespace {

constexpr uint32_t kSysconBase = 0x40048000UL;
constexpr uint32_t kSwmBase    = 0x4000C000UL;
constexpr uint32_t kUsart0Base = 0x40064000UL;

// SYSAHBCLKCTRL0 (SYSCON + 0x80) clock-enable bit positions.
constexpr uint32_t kClkGpio0  = 6;
constexpr uint32_t kClkSwm    = 7;
constexpr uint32_t kClkUsart0 = 14;
constexpr uint32_t kClkIocon  = 18;

// USART0 STAT (base + 0x08) / CFG (base + 0x00) flags.
constexpr uint32_t kUsartStatRxRdy = (1u << 0);
constexpr uint32_t kUsartStatTxRdy = (1u << 2);
constexpr uint32_t kUsartCfgEnable = (1u << 0);
constexpr uint32_t kUsartCfg8N1    = (1u << 2);

inline volatile uint32_t& reg32(uint32_t addr) {
    return *reinterpret_cast<volatile uint32_t*>(addr);
}

bool g_ready = false;

void usart0_init(uint32_t baud) {
    if (baud == 0u) {
        baud = 115200u;
    }

    // Enable peripheral clocks: GPIO0, SWM, IOCON, USART0.
    reg32(kSysconBase + 0x80) |= (1u << kClkGpio0) | (1u << kClkSwm) |
                                 (1u << kClkIocon) | (1u << kClkUsart0);

    // Select main clock as the USART0 function clock (FCLKSEL0 = 1).
    reg32(kSysconBase + 0x90) = 1u;

    // Reset USART0: PRESETCTRL0 bit 14 asserted low then released.
    reg32(kSysconBase + 0x88) &= ~(1u << 14);
    reg32(kSysconBase + 0x88) |= (1u << 14);

    // SWM PINASSIGN0: byte0 = U0_TXD -> P0_25, byte1 = U0_RXD -> P0_24,
    // bytes 2/3 (U0_RTS/U0_CTS) left unassigned (0xFF).
    reg32(kSwmBase + 0x00) = 0xFFFF0000u | (24u << 8) | 25u;

    reg32(kUsart0Base + 0x00) = 0u;   // CFG: disable while configuring
    reg32(kUsart0Base + 0x28) = 15u;  // OSR = 15 -> 16x oversample
    const uint32_t brg = (F_CPU + (baud * 8u)) / (baud * 16u);
    reg32(kUsart0Base + 0x20) = brg > 0u ? brg - 1u : 0u;  // BRG
    reg32(kUsart0Base + 0x00) = kUsartCfgEnable | kUsartCfg8N1;

    g_ready = true;
}

inline void usart0_ensure() {
    if (!g_ready) {
        usart0_init(115200u);
    }
}

inline void usart0_write(uint8_t byte) {
    while ((reg32(kUsart0Base + 0x08) & kUsartStatTxRdy) == 0u) {
    }
    reg32(kUsart0Base + 0x1C) = byte;  // TXDAT
}

size_t print_unsigned(unsigned long value, HardwareSerial& serial) {
    char buf[20];
    int i = 0;
    if (value == 0u) {
        buf[i++] = '0';
    }
    while (value > 0u) {
        buf[i++] = static_cast<char>('0' + (value % 10u));
        value /= 10u;
    }
    size_t written = 0;
    while (i > 0) {
        written += serial.write(static_cast<uint8_t>(buf[--i]));
    }
    return written;
}

size_t print_signed(long value, HardwareSerial& serial) {
    size_t written = 0;
    unsigned long magnitude;
    if (value < 0) {
        written += serial.write(static_cast<uint8_t>('-'));
        magnitude = static_cast<unsigned long>(-(value + 1)) + 1u;
    } else {
        magnitude = static_cast<unsigned long>(value);
    }
    return written + print_unsigned(magnitude, serial);
}

}  // namespace

HardwareSerial Serial;

void HardwareSerial::begin(uint32_t baud) {
    usart0_init(baud);
}

void HardwareSerial::end() {}

HardwareSerial::operator bool() const {
    return true;
}

int HardwareSerial::available() {
    if (!g_ready) {
        return 0;
    }
    return (reg32(kUsart0Base + 0x08) & kUsartStatRxRdy) ? 1 : 0;
}

int HardwareSerial::read() {
    if (!g_ready) {
        return -1;
    }
    if ((reg32(kUsart0Base + 0x08) & kUsartStatRxRdy) == 0u) {
        return -1;
    }
    return static_cast<int>(reg32(kUsart0Base + 0x14) & 0xFFu);  // RXDAT
}

int HardwareSerial::peek() {
    if (!g_ready) {
        return -1;
    }
    if ((reg32(kUsart0Base + 0x08) & kUsartStatRxRdy) == 0u) {
        return -1;
    }
    return static_cast<int>(reg32(kUsart0Base + 0x14) & 0xFFu);
}

void HardwareSerial::flush() {
    if (!g_ready) {
        return;
    }
    while ((reg32(kUsart0Base + 0x08) & kUsartStatTxRdy) == 0u) {
    }
}

size_t HardwareSerial::write(uint8_t value) {
    usart0_ensure();
    usart0_write(value);
    return 1;
}

size_t HardwareSerial::write(const uint8_t* buffer, size_t size) {
    if (!buffer) {
        return 0;
    }
    usart0_ensure();
    for (size_t i = 0; i < size; ++i) {
        usart0_write(buffer[i]);
    }
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
    return print_signed(value, *this);
}

size_t HardwareSerial::print(unsigned int value) {
    return print_unsigned(value, *this);
}

size_t HardwareSerial::print(long value) {
    return print_signed(value, *this);
}

size_t HardwareSerial::print(unsigned long value) {
    return print_unsigned(value, *this);
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

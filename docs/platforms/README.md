# Platforms

Board and platform support lives here.

## Canonical Support Matrix

[`docs/BOARD_STATUS.md`](../BOARD_STATUS.md) is the canonical board-status
document. It owns:

- Per-platform CI badges.
- Supported board and platform tables.
- Board-family notes.
- The process for adding a new board.

The root README intentionally keeps only a short supported-platform summary so
that board badges and board status do not drift between files.

## Supported Families

fbuild supports AVR, MegaAVR, Renesas RA, ESP8266, ESP32 variants, CH32
RISC-V, Teensy, STM32, SAM/SAMD, RP2040/RP2350, Nordic NRF52, Apollo3,
Silicon Labs EFR32, NXP LPC, and WASM via Emscripten.

## Emulator Support

Emulator backend behavior is documented in
[`docs/guides/emulator-testing.md`](../guides/emulator-testing.md). In short:

- ATmega328P defaults to `avr8js`.
- Other AVR MCUs with `simavr` in board metadata default to `simavr`.
- ESP32, ESP32-S3, ESP32-C3, ESP32-C6, and ESP32-H2 default to `qemu`.

Use `fbuild test-emu` for CI-friendly emulator runs.

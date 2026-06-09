# Emulator Testing

fbuild can build and run firmware in emulators without physical hardware.
There are two user-facing entry points:

- `fbuild test-emu` - build, emulate, stream output, and exit with the emulator
  result. This is the CI-friendly path.
- `fbuild deploy --to emu` - use the deploy flow and optionally open a monitor
  page or stream monitor output.

Both commands auto-detect the emulator backend from the selected board, or
accept `--emulator <backend>`.

## Quick Examples

```bash
# Auto-detect the emulator backend from the board.
fbuild test-emu tests/platform/uno -e uno

# Explicit backend with a timeout.
fbuild test-emu tests/platform/esp32s3 -e esp32s3 --emulator qemu --timeout 10

# AVR with simavr.
fbuild test-emu tests/platform/mega -e megaatmega2560 --emulator simavr

# Halt on the first test result pattern.
fbuild test-emu tests/platform/uno -e uno \
  --halt-on-success "TEST PASSED" --halt-on-error "TEST FAILED"
```

Deploy to an emulator:

```bash
fbuild deploy tests/platform/uno -e uno --to emu
fbuild deploy tests/platform/uno -e uno --to emu --monitor
fbuild deploy tests/platform/esp32s3 -e esp32s3 --to emu --emulator qemu --monitor --timeout 10
```

## Common Options

| Option | Description |
|---|---|
| `--emulator <backend>` | Force `avr8js`, `qemu`, or `simavr`. |
| `--timeout <secs>` | Stop the emulator after N seconds. |
| `--halt-on-success <regex>` | Stop and report success when output matches. |
| `--halt-on-error <regex>` | Stop and report failure when output matches. |
| `--expect <regex>` | Require this pattern in output; timeout fails if missing. |
| `--no-timestamp` | Disable timestamp prefixes on output lines. |
| `-v`, `--verbose` | Show emulator command and build details. |

## Backends

| Backend | Platforms | MCUs | Requirements |
|---|---|---|---|
| `avr8js` | AtmelAVR | ATmega328P | Node.js; fbuild includes the headless runner. |
| `simavr` | AtmelAVR, MegaAVR | ATmega2560, ATmega32U4, and others | `simavr` binary on `PATH`. |
| `qemu` | Espressif32 | ESP32, ESP32-S3, ESP32-C3, ESP32-C6, ESP32-H2 | Native QEMU; fbuild manages supported runtime packages. |

Auto-detection rules when `--emulator` is omitted:

- ATmega328P defaults to `avr8js`.
- Other AVR MCUs with `simavr` in `debug_tools` default to `simavr`.
- ESP32, ESP32-S3, ESP32-C3, ESP32-C6, and ESP32-H2 default to `qemu`.

The canonical board support matrix is [BOARD_STATUS.md](../BOARD_STATUS.md).

## QEMU Notes

ESP32-family QEMU runs from a normal Arduino environment. fbuild launches
`qemu-system-xtensa` for ESP32 and ESP32-S3, and `qemu-system-riscv32` for
ESP32-C3, ESP32-C6, and ESP32-H2. Required QEMU build flags are injected when
deploying to `--to emu`.

Example ESP32-S3 / ESP32-C3 settings:

```ini
[env:esp32s3]
platform = https://github.com/pioarduino/platform-espressif32/releases/download/55.03.34/platform-espressif32.zip
board = esp32-s3-devkitc-1
framework = arduino
board_build.flash_mode = dio
board_upload.flash_mode = dio

[env:esp32c3]
platform = https://github.com/pioarduino/platform-espressif32/releases/download/55.03.34/platform-espressif32.zip
board = esp32-c3-devkitm-1
framework = arduino
board_build.flash_mode = dio
board_upload.flash_mode = dio
```

QEMU requires DIO flash mode. Boards configured with `qio` or `qout` fail fast
before building.

Supported QEMU hosts are Linux x86_64/arm64, macOS x86_64/arm64, and Windows
x86_64. On Windows, fbuild stages the required QEMU runtime DLLs for the
managed install.

## Known Limitations

1. ESP32 QEMU supports ESP32, ESP32-S3, ESP32-C3, ESP32-C6, and ESP32-H2.
   ESP32-S2 and ESP32-P4 are not yet supported by upstream Espressif QEMU.
2. ESP32-S3 images are patched for QEMU to bypass an ADC calibration
   constructor that hangs under emulation; fbuild repairs the image checksum
   and hash after patching. RISC-V variants do not require this patch.
3. QEMU is slower than real hardware. Use it for functional validation, not
   timing-sensitive behavior.
4. Peripheral coverage is incomplete. Real hardware is still required for
   production validation.

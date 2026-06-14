# ESP32 deployer

Espressif chip family deployer for `fbuild deploy -e esp32*` (UART/USB-CDC
bootloader path).

Module layout:

- `mod.rs` — `Esp32Deployer` orchestration entry point and `Deployer` trait
  implementation.
- `deployer.rs` — esptool/native-flasher invocation, stub upload, reset-after-
  flash handshake.
- `image.rs` — bootloader / partition-table / firmware region layout helpers
  (offsets, padding, MD5 derivation).
- `qemu.rs` — QEMU flash-image assembly path (`fbuild test-emu` against the
  `esp32-qemu` emulator runner).
- `verify.rs` — post-flash verification: native `FLASH_MD5SUM` round-trip with
  esptool subprocess as the runtime fallback (PR #66 / `espflash-native`
  feature).
- `parse.rs` — esptool stdout / stderr parsers that surface progress events and
  bootloader handshake errors to the daemon's WebSocket clients.
- `tests.rs` — unit tests for the ESP32-S3 QEMU flash sizing, deployer
  construction from `BoardConfig`, and parse helpers.

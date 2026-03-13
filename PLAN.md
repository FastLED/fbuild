# Platform-by-Platform Migration Plan

## Strategy

Vertical slices by platform: implement the minimum of each crate needed for one platform, validate end-to-end, then widen.

```
Platform 1 (AVR):    config[avr] → packages[avr] → build[avr] → deploy[avr] → test
Platform 2 (ESP32):  config[esp32] → packages[esp32] → build[esp32] → deploy[esp32] → test
Platform 3+:         same pattern for Teensy, RP2040, STM32, ESP8266, WASM
```

## Platform Priority

| # | Platform | Test Projects |
|---|----------|---------------|
| 1 | **AVR** | tests/uno, tests/uno_minimal, tests/uno_simple |
| 2 | **ESP32** | tests/esp32dev, tests/esp32c6, tests/esp32s3 |
| 3 | **Teensy** | tests/teensy40, tests/teensy41 |
| 4 | **RP2040** | tests/rpipico, tests/rpipico2 |
| 5 | **STM32** | tests/bluepill_f103c8, tests/nucleo_f446re |
| 6 | **ESP8266** | tests/esp8266 |
| 7 | **WASM** | tests/wasm |

## Completed: Shared Foundation

- [x] fbuild-core: SubprocessRunner, ToolOutput, SizeInfo::parse(), Platform::from_platform_str
- [x] fbuild-config: Full INI parser (extends inheritance, variable substitution, multi-line values)
- [x] fbuild-config: BoardConfig (from_boards_txt, from_board_id with 14 boards, get_defines, platform detection)
- [x] fbuild-packages: Cache (URL hashing, directory management, build dirs)
- [x] fbuild-packages: Base traits (Package, Toolchain, Framework) + PackageBase with async staged_install
- [x] fbuild-packages: Async HTTP downloader + parallel download_all + SHA256 checksum verification
- [x] fbuild-packages: Archive extractors (tar.gz, tar.bz2, zip)
- [x] fbuild-build: SourceScanner (.ino preprocessing, function prototype extraction, #line directives)
- [x] fbuild-build: Compiler/Linker traits + CompilerBase + LinkerBase

## Completed: Platform 1 — AVR

- [x] fbuild-packages: AvrToolchain (avr-gcc 7.3.0, platform-specific URLs, bin path resolution)
- [x] fbuild-packages: ArduinoCore (Arduino AVR core 1.8.6 from GitHub, cores/variants/boards.txt)
- [x] fbuild-build: AvrCompiler (avr-gcc/g++ flags, MCU/define/include flag building)
- [x] fbuild-build: AvrLinker (avr-ar, avr-gcc link, avr-objcopy → .hex, avr-size reporting)
- [x] fbuild-build: AvrOrchestrator (full build pipeline: config → packages → compile → link → size)
- [x] fbuild-deploy: AvrDeployer (avrdude invocation with port/MCU/protocol/baud)

### AVR Validation (Pending Hardware)

- [ ] `fbuild build tests/uno -e uno` produces firmware.hex
- [ ] firmware.hex byte-identical to Python fbuild output
- [ ] Size info matches Python fbuild output
- [ ] `fbuild deploy tests/uno -e uno --port COM3` works on real hardware

## Next: Platform 2 — ESP32

- [ ] fbuild-packages: Esp32Toolchain (xtensa-esp32-elf-gcc / riscv32)
- [ ] fbuild-packages: Esp32Framework (pioarduino platform-espressif32, ESP-IDF libs)
- [ ] fbuild-build: Esp32Orchestrator (partition tables, bootloader merge)
- [ ] fbuild-deploy: Esp32Deployer (esptool.py, flash offsets, crash-loop recovery)
- [ ] fbuild-serial: Real serialport I/O (USB-CDC, Windows 30-retry, preemption)

## Daemon (Parallel Track)

Follow zccache's daemon launch pattern — not porting the Python daemon launch approach.

- [ ] fbuild-daemon: Axum HTTP server (same endpoints as Python daemon)
- [ ] fbuild-daemon: WebSocket serial monitor
- [ ] fbuild-daemon: Request processors

## PyO3 Bindings (After ESP32)

- [ ] fbuild-python: Wire SerialMonitor/DaemonConnection to real backends
- [ ] Test with FastLED's ci/debug_attached.py

## Test Count: 125 passing across 11 crates

| Crate | Tests |
|-------|-------|
| fbuild-core | 11 (subprocess, SizeInfo, Platform) |
| fbuild-config | 44 (INI parser 26, BoardConfig 18) |
| fbuild-packages | 33 (cache 16, downloader 2, extractor 1, AvrToolchain 6, ArduinoCore 5, README 3) |
| fbuild-build | 33 (source_scanner 14, compiler 4, linker 2, AVR compiler 6, AVR linker 1, AVR orchestrator 3, README 3) |
| fbuild-deploy | 3 (AVR deployer) |
| fbuild-paths | 1 |

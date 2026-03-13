# TODO — Platform-by-Platform Migration

## Completed: Shared Foundation

- [x] fbuild-core: SubprocessRunner, ToolOutput, SizeInfo::parse()
- [x] fbuild-config: Full INI parser (extends, variable substitution)
- [x] fbuild-config: BoardConfig (from_boards_txt, from_board_id, get_defines)
- [x] fbuild-packages: Cache system (URL hashing, directory management)
- [x] fbuild-packages: Base traits (Package, Toolchain, Framework)
- [x] fbuild-packages: Async HTTP downloader + archive extractors
- [x] fbuild-build: SourceScanner (.ino preprocessing, prototype extraction)
- [x] fbuild-build: Base traits (Compiler, Linker, CompilerBase, LinkerBase)

## Completed: Platform 1 — AVR Code

- [x] fbuild-packages: AvrToolchain (download avr-gcc, resolve bin paths)
- [x] fbuild-packages: ArduinoCore (download Arduino AVR core)
- [x] fbuild-build: AvrCompiler (avr-gcc flags, compile C/C++)
- [x] fbuild-build: AvrLinker (link, objcopy, size reporting)
- [x] fbuild-build: AvrOrchestrator (wire all phases)
- [x] fbuild-deploy: AvrDeployer (avrdude invocation)

## Current: AVR Validation

- [ ] Wire CLI `build` command to invoke AvrOrchestrator
- [ ] `fbuild build tests/uno -e uno` produces firmware.hex
- [ ] firmware.hex byte-identical to Python fbuild output
- [ ] Size info matches Python fbuild output

## Next: Platform 2 — ESP32

- [ ] fbuild-packages: Esp32Toolchain, Esp32Framework
- [ ] fbuild-build: Esp32Orchestrator
- [ ] fbuild-deploy: Esp32Deployer
- [ ] fbuild-serial: Real serialport I/O

## Future: Platforms 3-7

- [ ] Teensy, RP2040, STM32, ESP8266, WASM

## Future: Daemon + PyO3

- [ ] fbuild-daemon: Axum server (follow zccache pattern for launch)
- [ ] fbuild-python: Wire PyO3 bindings to real backends

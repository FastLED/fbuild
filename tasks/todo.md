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

## Completed: Platform 1 — AVR

- [x] fbuild-packages: AvrToolchain (download avr-gcc, resolve bin paths)
- [x] fbuild-packages: ArduinoCore (download Arduino AVR core)
- [x] fbuild-build: AvrCompiler (avr-gcc flags, compile C/C++)
- [x] fbuild-build: AvrLinker (link, objcopy, size reporting)
- [x] fbuild-build: AvrOrchestrator (wire all phases)
- [x] fbuild-deploy: AvrDeployer (avrdude invocation)

## Completed: Platform 2 — ESP32

- [x] fbuild-packages: Esp32Toolchain, Esp32Framework
- [x] fbuild-build: Esp32Orchestrator
- [x] fbuild-deploy: Esp32Deployer
- [x] fbuild-serial: Real serialport I/O

## Completed: Platform 3 — Teensy

- [x] fbuild-packages: ArmToolchain, TeensyCores
- [x] fbuild-build: TeensyOrchestrator
- [x] fbuild-deploy: TeensyDeployer

## Completed: Build Pipeline Normalization

- [x] Phase 1: Bug fixes (esptool, BuildProfile, firmware_path rename)
- [x] Phase 2: Shared pipeline.rs helpers (BuildContext, compile/link/result helpers)
- [x] Phase 3: Compiler trait extension + run_sequential_build() for AVR/Teensy

## Future: Platforms 4-7

- [ ] RP2040, STM32, ESP8266, WASM

## Future: Daemon + PyO3

- [ ] fbuild-daemon: Axum server (follow zccache pattern for launch)
- [ ] fbuild-python: Wire PyO3 bindings to real backends

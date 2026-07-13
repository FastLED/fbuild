# fbuild

![fbuild](https://github.com/user-attachments/assets/7db78eba-b10f-44c7-ae32-7fc0b5e46642)

`fbuild` is a fast, multi-platform compiler, deployer, emulator runner, and
serial monitor for embedded development. It reads the same `platformio.ini`
files already used by PlatformIO sketches, but uses a Rust-native, data-driven
build pipeline.

[![Check Ubuntu](https://github.com/fastled/fbuild/actions/workflows/check-ubuntu.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/check-ubuntu.yml)
[![Check macOS](https://github.com/fastled/fbuild/actions/workflows/check-macos.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/check-macos.yml)
[![Check Windows](https://github.com/fastled/fbuild/actions/workflows/check-windows.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/check-windows.yml)
[![Formatting](https://github.com/fastled/fbuild/actions/workflows/fmt.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/fmt.yml)
[![Documentation](https://github.com/fastled/fbuild/actions/workflows/docs.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/docs.yml)
[![Build Native Binaries](https://github.com/fastled/fbuild/actions/workflows/build.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build.yml)

## Build Matrix

These board builds are part of the front door. They show the platform breadth
that fbuild actively protects in CI.

### AVR

[![Build Arduino Uno](https://github.com/fastled/fbuild/actions/workflows/build-uno.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-uno.yml)
[![Build Leonardo](https://github.com/fastled/fbuild/actions/workflows/build-leonardo.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-leonardo.yml)
[![Build ATmega8A](https://github.com/fastled/fbuild/actions/workflows/build-atmega8a.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-atmega8a.yml)
[![Build ATtiny85](https://github.com/fastled/fbuild/actions/workflows/build-attiny85.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-attiny85.yml)
[![Build ATtiny88](https://github.com/fastled/fbuild/actions/workflows/build-attiny88.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-attiny88.yml)
[![Build ATtiny4313](https://github.com/fastled/fbuild/actions/workflows/build-attiny4313.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-attiny4313.yml)

### MegaAVR

[![Build ATtiny1604](https://github.com/fastled/fbuild/actions/workflows/build-attiny1604.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-attiny1604.yml)
[![Build ATtiny1616](https://github.com/fastled/fbuild/actions/workflows/build-attiny1616.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-attiny1616.yml)
[![Build Nano Every](https://github.com/fastled/fbuild/actions/workflows/build-nano_every.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-nano_every.yml)

### Renesas

[![Build UNO R4 WiFi](https://github.com/fastled/fbuild/actions/workflows/build-uno_r4_wifi.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-uno_r4_wifi.yml)

### ESP8266

[![Build ESP8266](https://github.com/fastled/fbuild/actions/workflows/build-esp8266.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp8266.yml)

### ESP32

[![Build ESP32 Dev](https://github.com/fastled/fbuild/actions/workflows/build-esp32dev.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32dev.yml)
[![Build ESP32-C2](https://github.com/fastled/fbuild/actions/workflows/build-esp32c2.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c2.yml)
[![Build ESP32-C3](https://github.com/fastled/fbuild/actions/workflows/build-esp32c3.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c3.yml)
[![Build ESP32-C5](https://github.com/fastled/fbuild/actions/workflows/build-esp32c5.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c5.yml)
[![Build ESP32-C6](https://github.com/fastled/fbuild/actions/workflows/build-esp32c6.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c6.yml)
[![Build ESP32-H2](https://github.com/fastled/fbuild/actions/workflows/build-esp32h2.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32h2.yml)
[![Build ESP32-P4](https://github.com/fastled/fbuild/actions/workflows/build-esp32p4.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32p4.yml)
[![Build ESP32-S2](https://github.com/fastled/fbuild/actions/workflows/build-esp32s2.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32s2.yml)
[![Build ESP32-S3](https://github.com/fastled/fbuild/actions/workflows/build-esp32s3.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-esp32s3.yml)

### CH32V (RISC-V)

[![Build CH32V003](https://github.com/fastled/fbuild/actions/workflows/build-ch32v003.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v003.yml)
[![Build CH32V103](https://github.com/fastled/fbuild/actions/workflows/build-ch32v103.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v103.yml)
[![Build CH32V203](https://github.com/fastled/fbuild/actions/workflows/build-ch32v203.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v203.yml)
[![Build CH32V208](https://github.com/fastled/fbuild/actions/workflows/build-ch32v208.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v208.yml)
[![Build CH32V303](https://github.com/fastled/fbuild/actions/workflows/build-ch32v303.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v303.yml)
[![Build CH32V307](https://github.com/fastled/fbuild/actions/workflows/build-ch32v307.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v307.yml)

### CH32X (RISC-V, USB PD)

[![Build CH32X035](https://github.com/fastled/fbuild/actions/workflows/build-ch32x035.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-ch32x035.yml)

### Teensy

[![Build Teensy 4.1](https://github.com/fastled/fbuild/actions/workflows/build-teensy41.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensy41.yml)
[![Build Teensy 4.0](https://github.com/fastled/fbuild/actions/workflows/build-teensy40.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensy40.yml)
[![Build Teensy 3.6](https://github.com/fastled/fbuild/actions/workflows/build-teensy36.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensy36.yml)
[![Build Teensy 3.5](https://github.com/fastled/fbuild/actions/workflows/build-teensy35.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensy35.yml)
[![Build Teensy 3.2](https://github.com/fastled/fbuild/actions/workflows/build-teensy32.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensy32.yml)
[![Build Teensy 3.1](https://github.com/fastled/fbuild/actions/workflows/build-teensy31.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensy31.yml)
[![Build Teensy 3.0](https://github.com/fastled/fbuild/actions/workflows/build-teensy30.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensy30.yml)
[![Build Teensy LC](https://github.com/fastled/fbuild/actions/workflows/build-teensylc.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-teensylc.yml)

### STM32

[![Build STM32F103C8](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103c8.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103c8.yml)
[![Build STM32F103CB](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103cb.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103cb.yml)
[![Build STM32F103TB](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103tb.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103tb.yml)
[![Build STM32F411CE](https://github.com/fastled/fbuild/actions/workflows/build-stm32f411ce.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-stm32f411ce.yml)
[![Build STM32H747XI](https://github.com/fastled/fbuild/actions/workflows/build-stm32h747xi.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-stm32h747xi.yml)
[![Build Nucleo F429ZI](https://github.com/fastled/fbuild/actions/workflows/build-nucleo_f429zi.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-nucleo_f429zi.yml)
[![Build Nucleo F439ZI](https://github.com/fastled/fbuild/actions/workflows/build-nucleo_f439zi.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-nucleo_f439zi.yml)
[![Build Arduino Giga R1](https://github.com/fastled/fbuild/actions/workflows/build-giga-r1.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-giga-r1.yml)

### SAM / SAMD

[![Build Arduino Due](https://github.com/fastled/fbuild/actions/workflows/build-sam3x8e_due.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-sam3x8e_due.yml)
[![Build SAMD21](https://github.com/fastled/fbuild/actions/workflows/build-samd21.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-samd21.yml)
[![Build Arduino Zero](https://github.com/fastled/fbuild/actions/workflows/build-samd21_zero.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-samd21_zero.yml)
[![Build SAMD51J](https://github.com/fastled/fbuild/actions/workflows/build-samd51j.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-samd51j.yml)
[![Build SAMD51P](https://github.com/fastled/fbuild/actions/workflows/build-samd51p.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-samd51p.yml)

### RP2040 / RP2350

[![Build RP2040](https://github.com/fastled/fbuild/actions/workflows/build-rp2040.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-rp2040.yml)
[![Build RP2350](https://github.com/fastled/fbuild/actions/workflows/build-rp2350.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-rp2350.yml)

### Nordic NRF52

[![Build nRF52840 DK](https://github.com/fastled/fbuild/actions/workflows/build-nrf52840_dk.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-nrf52840_dk.yml)
[![Build SuperMini nRF52840](https://github.com/fastled/fbuild/actions/workflows/build-supermini_nrf52840.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-supermini_nrf52840.yml)
[![Build nice!nano nRF52840](https://github.com/fastled/fbuild/actions/workflows/build-nice_nano_nrf52840.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-nice_nano_nrf52840.yml)
[![Build nRFMicro nRF52840](https://github.com/fastled/fbuild/actions/workflows/build-nrfmicro_nrf52840.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-nrfmicro_nrf52840.yml)
[![Build Adafruit Feather NRF52840 Sense](https://github.com/fastled/fbuild/actions/workflows/build-nrf52840-sense.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-nrf52840-sense.yml)

### Apollo3

[![Build Apollo3 RedBoard](https://github.com/fastled/fbuild/actions/workflows/build-apollo3_red.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-apollo3_red.yml)
[![Build Apollo3 expLoRaBLE](https://github.com/fastled/fbuild/actions/workflows/build-apollo3_thing_explorable.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-apollo3_thing_explorable.yml)

### NXP LPC (Cortex-M0+)

[![Build LPC804](https://github.com/fastled/fbuild/actions/workflows/build-lpc804.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-lpc804.yml)
[![Build LPC845](https://github.com/fastled/fbuild/actions/workflows/build-lpc845.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-lpc845.yml)
[![Build LPC845-BRK](https://github.com/fastled/fbuild/actions/workflows/build-lpc845brk.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-lpc845brk.yml)
[![Build LPCXpresso804](https://github.com/fastled/fbuild/actions/workflows/build-lpcxpresso804.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-lpcxpresso804.yml)
[![Build LPCXpresso845-MAX](https://github.com/fastled/fbuild/actions/workflows/build-lpcxpresso845max.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-lpcxpresso845max.yml)

### Silicon Labs

[![Build MGM240](https://github.com/fastled/fbuild/actions/workflows/build-mgm240.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-mgm240.yml)
[![Build SparkFun Thing Plus Matter](https://github.com/fastled/fbuild/actions/workflows/build-thingplusmatter.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-thingplusmatter.yml)

### Raspberry Pi Pico

[![Build Raspberry Pi Pico](https://github.com/fastled/fbuild/actions/workflows/build-rpipico.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-rpipico.yml)
[![Build Raspberry Pi Pico 2](https://github.com/fastled/fbuild/actions/workflows/build-rpipico2.yml/badge.svg)](https://github.com/fastled/fbuild/actions/workflows/build-rpipico2.yml)

Board descriptions and family deep-dives live in
[`docs/BOARD_STATUS.md`](docs/BOARD_STATUS.md).

## Installation

```bash
pip install fbuild
```

For source installs, platform notes, and first-run cache behavior, start with
the [getting started guide](docs/getting-started/README.md).

## Command Quick Start

fbuild reads the same `platformio.ini` files as PlatformIO. Use these commands
as direct replacements for the most common PlatformIO workflows:

| fbuild | PlatformIO equivalent | Use it to |
|---|---|---|
| `fbuild build` | `pio run` | Compile the project. |
| `fbuild build --clean` | `pio run --target clean`, then `pio run` | Clean and compile the project. |
| `fbuild deploy` | `pio run --target upload` | Build and upload firmware. |
| `fbuild deploy --clean` | Clean, then `pio run --target upload` | Clean, build, and upload firmware. |
| `fbuild monitor` | `pio device monitor` | Monitor serial output without flashing. |
| `fbuild ci` | `pio ci` | Build one or more sketches for CI. |

Pass `--platformio` to `build`, `deploy`, or `monitor` to delegate that
workflow to the installed PlatformIO CLI. `fbuild ci` is a fbuild-native,
PlatformIO-compatible CI command; detailed flags and nested commands are in
the [CLI reference](docs/reference/cli.md).

### fbuild-only commands

These commands extend beyond the PlatformIO workflow surface:

| Command | Purpose |
|---|---|
| `fbuild symbols` | Report per-symbol firmware size and bloat details. |
| `fbuild bloat` | Inspect symbol back-references and generate bloat graphs. |
| `fbuild reset` | Reset a device without flashing it. |
| `fbuild purge` | Purge downloaded packages or run cache garbage collection. |
| `fbuild sync` | Resolve `platformio.ini` dependencies into a deterministic lock file. |
| `fbuild daemon` | Manage the background build daemon, locks, and cache. |
| `fbuild show` | Show daemon logs and other runtime information. |
| `fbuild device` | List devices and manage device leases. |
| `fbuild mcp` | Start the Model Context Protocol server for AI integrations. |
| `fbuild clang-tidy` | Run clang-tidy static analysis on project sources. |
| `fbuild iwyu` | Run include-what-you-use analysis on project sources. |
| `fbuild clangd-config` | Generate clangd and VS Code configuration for the project. |
| `fbuild test-emu` | Build and run firmware in an emulator for testing. |
| `fbuild clang-query` | Run a clang-query matcher against project sources. |
| `fbuild lnk` | Fetch, verify, and create `.lnk` resource pointers. |
| `fbuild lib-select` | Diagnose the LDF-style library selection result. |
| `fbuild compile-many` | Compile many sketches against one board in parallel stages. |
| `fbuild serial` | Probe serial ports and read them with board-aware settings. |
| `fbuild bringup` | Orchestrate build, flash, reset, monitor, and bring-up steps. |
| `fbuild port` | Scan serial ports with vendor and product identification. |
| `fbuild cache` | Save, restore, list, and verify portable cache archives. |

See `fbuild help <command>` or the [full CLI reference](docs/reference/cli.md)
for options and nested subcommands.

## Quick Start

Create a minimal Arduino project:

```bash
mkdir my-project
cd my-project
mkdir src
```

Add `platformio.ini`:

```ini
[env:uno]
platform = atmelavr
board = uno
framework = arduino
```

Add `src/main.ino`:

```cpp
void setup() {
  pinMode(LED_BUILTIN, OUTPUT);
}

void loop() {
  digitalWrite(LED_BUILTIN, HIGH);
  delay(1000);
  digitalWrite(LED_BUILTIN, LOW);
  delay(1000);
}
```

Build it:

```bash
fbuild build
```

On the first build, fbuild downloads the toolchain and framework packages it
needs, then caches them for later builds. A successful Uno build writes
`.fbuild/build/uno/firmware.hex`.

## Examples

Common workflows:

```bash
fbuild build
fbuild deploy --clean
fbuild deploy --monitor
fbuild test-emu . -e uno
fbuild monitor --timeout 60 --halt-on-success "TEST PASSED"
```

Detailed build, deploy, monitor, and emulator examples live in the
[CLI reference](docs/reference/cli.md) and
[emulator testing guide](docs/guides/emulator-testing.md).

## Docs Index

The full FAQ-style map is [`docs/INDEX.md`](docs/INDEX.md). Common entry
points:

| Goal | Start here |
|---|---|
| Install fbuild and run the first build | [`docs/getting-started/`](docs/getting-started/README.md) |
| Use build, deploy, monitor, or test-emu | [`docs/reference/cli.md`](docs/reference/cli.md) |
| Configure `platformio.ini` | [`docs/reference/platformio-ini.md`](docs/reference/platformio-ini.md) |
| Check board and platform support | [`docs/platforms/`](docs/platforms/README.md) |
| Understand the project rationale | [`docs/WHY.md`](docs/WHY.md) |
| Work on fbuild itself | [`docs/development/`](docs/development/README.md) |
| Read architecture internals | [`docs/architecture/overview.md`](docs/architecture/overview.md) |

## Key Features

- `platformio.ini` compatibility for existing Arduino and ESP32 sketches
- Fast incremental builds with cached toolchains, frameworks, and libraries
- URL-based package and library management, including GitHub `lib_deps`
- Build, deploy, serial monitor, and emulator test workflows from one CLI
- Cross-platform support on Windows, macOS, and Linux
- Transparent architecture with Rust workspace internals documented under
  [`docs/architecture/`](docs/architecture/README.md)

See [`docs/WHY.md`](docs/WHY.md) for the full rationale, benefits, and
performance notes.

## CLI Usage

The user-facing command reference is
[`docs/reference/cli.md`](docs/reference/cli.md). It covers core workflows
(`build`, `deploy`, `monitor`, `test-emu`) and diagnostics such as `symbols`,
`bloat`, `lib-select`, `compile-many`, and `ci`.

## Configuration

fbuild reads `platformio.ini` project files. The configuration reference,
including `default_envs`, `build_flags`, `lib_deps`, upload and monitor
settings, and compatibility notes, lives in
[`docs/reference/platformio-ini.md`](docs/reference/platformio-ini.md).

## Emulator Testing

fbuild can build firmware and run it without hardware via `fbuild test-emu` or
`fbuild deploy --to emu`. Emulator backends, auto-detection rules, QEMU notes,
and known limitations live in
[`docs/guides/emulator-testing.md`](docs/guides/emulator-testing.md).

## PlatformIO Compatibility: `.eh_frame` Strip

On supported release builds, fbuild may strip unused GCC `.eh_frame` unwind
metadata to reduce firmware size. The policy, opt-out controls, and rationale
are documented in
[`docs/reference/platformio-compatibility.md`](docs/reference/platformio-compatibility.md).

## Supported Platforms

fbuild supports AVR, MegaAVR, Renesas RA, ESP8266, ESP32 variants, CH32
RISC-V, Teensy, STM32, SAM/SAMD, RP2040/RP2350, Nordic NRF52, Apollo3,
Silicon Labs EFR32, NXP LPC, and WASM via Emscripten.

For the canonical per-board CI badge matrix, support table, and board-family
notes, see [`docs/BOARD_STATUS.md`](docs/BOARD_STATUS.md) or the
[platforms docs](docs/platforms/README.md).

## Project Structure

The repository is a Rust workspace with a Python package boundary. The human
development guide is [`docs/development/`](docs/development/README.md), and the
crate dependency map is [`crates/CLAUDE.md`](crates/CLAUDE.md).

## Architecture

Architecture docs are decentralized under
[`docs/architecture/`](docs/architecture/README.md). Start with
[`docs/architecture/overview.md`](docs/architecture/overview.md), then follow
the subsystem-specific docs.

## Development

Testing, troubleshooting, linting, release, and local setup instructions live
in [`docs/development/`](docs/development/README.md). Project-wide rules for
contributors and LLM agents are in [`CLAUDE.md`](CLAUDE.md).

## License

In the spirit of Dan Garcia's permissively licensed software, `fbuild` is
presented as free software.

BSD 3-Clause License

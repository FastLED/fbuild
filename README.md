# fbuild

![fbuild](https://github.com/user-attachments/assets/7db78eba-b10f-44c7-ae32-7fc0b5e46642)

`fbuild` is a fast, multi-platform compiler, deployer, emulator runner, and
serial monitor for embedded development. It reads the same `platformio.ini`
files already used by PlatformIO sketches, but uses a Rust-native, data-driven
build pipeline.

[![Check Ubuntu](https://github.com/fastled/fbuild/actions/workflows/check-ubuntu.yml/badge.svg?branch=main&event=push)](https://github.com/fastled/fbuild/actions/workflows/check-ubuntu.yml)
[![Check Windows](https://github.com/fastled/fbuild/actions/workflows/check-windows.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/check-windows.yml)
[![Formatting](https://github.com/fastled/fbuild/actions/workflows/fmt.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/fmt.yml)
[![Documentation](https://github.com/fastled/fbuild/actions/workflows/docs.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/docs.yml)
[![Build Native Binaries](https://github.com/fastled/fbuild/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build.yml)

## Build Matrix

These board builds are part of the front door. They show the platform breadth
that fbuild actively protects in CI.

### AVR

[![Build Arduino Uno](https://github.com/fastled/fbuild/actions/workflows/build-uno.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-uno.yml)
[![Build Leonardo](https://github.com/fastled/fbuild/actions/workflows/build-leonardo.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-leonardo.yml)
[![Build ATmega8A](https://github.com/fastled/fbuild/actions/workflows/build-atmega8a.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-atmega8a.yml)
[![Build ATtiny85](https://github.com/fastled/fbuild/actions/workflows/build-attiny85.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-attiny85.yml)
[![Build ATtiny88](https://github.com/fastled/fbuild/actions/workflows/build-attiny88.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-attiny88.yml)
[![Build ATtiny4313](https://github.com/fastled/fbuild/actions/workflows/build-attiny4313.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-attiny4313.yml)

### MegaAVR

[![Build ATtiny1604](https://github.com/fastled/fbuild/actions/workflows/build-attiny1604.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-attiny1604.yml)
[![Build ATtiny1616](https://github.com/fastled/fbuild/actions/workflows/build-attiny1616.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-attiny1616.yml)
[![Build Nano Every](https://github.com/fastled/fbuild/actions/workflows/build-nano_every.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-nano_every.yml)

### Renesas

[![Build UNO R4 WiFi](https://github.com/fastled/fbuild/actions/workflows/build-uno_r4_wifi.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-uno_r4_wifi.yml)

### ESP8266

[![Build ESP8266](https://github.com/fastled/fbuild/actions/workflows/build-esp8266.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-esp8266.yml)

### ESP32

[![Build ESP32 Dev](https://github.com/fastled/fbuild/actions/workflows/build-esp32dev.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-esp32dev.yml)
[![Build ESP32-C2](https://github.com/fastled/fbuild/actions/workflows/build-esp32c2.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c2.yml)
[![Build ESP32-C3](https://github.com/fastled/fbuild/actions/workflows/build-esp32c3.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c3.yml)
[![Build ESP32-C5](https://github.com/fastled/fbuild/actions/workflows/build-esp32c5.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c5.yml)
[![Build ESP32-C6](https://github.com/fastled/fbuild/actions/workflows/build-esp32c6.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-esp32c6.yml)
[![Build ESP32-H2](https://github.com/fastled/fbuild/actions/workflows/build-esp32h2.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-esp32h2.yml)
[![Build ESP32-P4](https://github.com/fastled/fbuild/actions/workflows/build-esp32p4.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-esp32p4.yml)
[![Build ESP32-S2](https://github.com/fastled/fbuild/actions/workflows/build-esp32s2.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-esp32s2.yml)
[![Build ESP32-S3](https://github.com/fastled/fbuild/actions/workflows/build-esp32s3.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-esp32s3.yml)

### CH32V (RISC-V)

[![Build CH32V003](https://github.com/fastled/fbuild/actions/workflows/build-ch32v003.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v003.yml)
[![Build CH32V103](https://github.com/fastled/fbuild/actions/workflows/build-ch32v103.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v103.yml)
[![Build CH32V203](https://github.com/fastled/fbuild/actions/workflows/build-ch32v203.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v203.yml)
[![Build CH32V208](https://github.com/fastled/fbuild/actions/workflows/build-ch32v208.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v208.yml)
[![Build CH32V303](https://github.com/fastled/fbuild/actions/workflows/build-ch32v303.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v303.yml)
[![Build CH32V307](https://github.com/fastled/fbuild/actions/workflows/build-ch32v307.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-ch32v307.yml)

### CH32X (RISC-V, USB PD)

[![Build CH32X035](https://github.com/fastled/fbuild/actions/workflows/build-ch32x035.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-ch32x035.yml)

### Teensy

[![Build Teensy 4.1](https://github.com/fastled/fbuild/actions/workflows/build-teensy41.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-teensy41.yml)
[![Build Teensy 4.0](https://github.com/fastled/fbuild/actions/workflows/build-teensy40.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-teensy40.yml)
[![Build Teensy 3.6](https://github.com/fastled/fbuild/actions/workflows/build-teensy36.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-teensy36.yml)
[![Build Teensy 3.5](https://github.com/fastled/fbuild/actions/workflows/build-teensy35.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-teensy35.yml)
[![Build Teensy 3.2](https://github.com/fastled/fbuild/actions/workflows/build-teensy32.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-teensy32.yml)
[![Build Teensy 3.1](https://github.com/fastled/fbuild/actions/workflows/build-teensy31.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-teensy31.yml)
[![Build Teensy 3.0](https://github.com/fastled/fbuild/actions/workflows/build-teensy30.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-teensy30.yml)
[![Build Teensy LC](https://github.com/fastled/fbuild/actions/workflows/build-teensylc.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-teensylc.yml)

### STM32

[![Build STM32F103C8](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103c8.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103c8.yml)
[![Build STM32F103CB](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103cb.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103cb.yml)
[![Build STM32F103TB](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103tb.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-stm32f103tb.yml)
[![Build STM32F411CE](https://github.com/fastled/fbuild/actions/workflows/build-stm32f411ce.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-stm32f411ce.yml)
[![Build STM32H747XI](https://github.com/fastled/fbuild/actions/workflows/build-stm32h747xi.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-stm32h747xi.yml)
[![Build Nucleo F429ZI](https://github.com/fastled/fbuild/actions/workflows/build-nucleo_f429zi.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-nucleo_f429zi.yml)
[![Build Nucleo F439ZI](https://github.com/fastled/fbuild/actions/workflows/build-nucleo_f439zi.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-nucleo_f439zi.yml)
[![Build Arduino Giga R1](https://github.com/fastled/fbuild/actions/workflows/build-giga-r1.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-giga-r1.yml)

### SAM / SAMD / SAME

[![Build Arduino Due](https://github.com/fastled/fbuild/actions/workflows/build-sam3x8e_due.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-sam3x8e_due.yml)
[![Build SAMD21](https://github.com/fastled/fbuild/actions/workflows/build-samd21.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-samd21.yml)
[![Build Arduino Zero](https://github.com/fastled/fbuild/actions/workflows/build-samd21_zero.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-samd21_zero.yml)
[![Build SAMD51J](https://github.com/fastled/fbuild/actions/workflows/build-samd51j.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-samd51j.yml)
[![Build SAMD51P](https://github.com/fastled/fbuild/actions/workflows/build-samd51p.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-samd51p.yml)
[![Build Teknic ClearCore SAME53](https://github.com/fastled/fbuild/actions/workflows/build-clearcore.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-clearcore.yml)

### RP2040 / RP2350

[![Build RP2040](https://github.com/fastled/fbuild/actions/workflows/build-rp2040.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-rp2040.yml)
[![Build RP2350](https://github.com/fastled/fbuild/actions/workflows/build-rp2350.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-rp2350.yml)

### Nordic NRF52

[![Build nRF52840 DK](https://github.com/fastled/fbuild/actions/workflows/build-nrf52840_dk.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-nrf52840_dk.yml)
[![Build SuperMini nRF52840](https://github.com/fastled/fbuild/actions/workflows/build-supermini_nrf52840.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-supermini_nrf52840.yml)
[![Build nice!nano nRF52840](https://github.com/fastled/fbuild/actions/workflows/build-nice_nano_nrf52840.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-nice_nano_nrf52840.yml)
[![Build nRFMicro nRF52840](https://github.com/fastled/fbuild/actions/workflows/build-nrfmicro_nrf52840.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-nrfmicro_nrf52840.yml)
[![Build Adafruit Feather NRF52840 Sense](https://github.com/fastled/fbuild/actions/workflows/build-nrf52840-sense.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-nrf52840-sense.yml)

### Apollo3

[![Build Apollo3 RedBoard](https://github.com/fastled/fbuild/actions/workflows/build-apollo3_red.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-apollo3_red.yml)
[![Build Apollo3 expLoRaBLE](https://github.com/fastled/fbuild/actions/workflows/build-apollo3_thing_explorable.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-apollo3_thing_explorable.yml)

### NXP LPC (Cortex-M0+)

[![Build LPC804](https://github.com/fastled/fbuild/actions/workflows/build-lpc804.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-lpc804.yml)
[![Build LPC845](https://github.com/fastled/fbuild/actions/workflows/build-lpc845.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-lpc845.yml)
[![Build LPC845-BRK](https://github.com/fastled/fbuild/actions/workflows/build-lpc845brk.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-lpc845brk.yml)
[![Build LPCXpresso804](https://github.com/fastled/fbuild/actions/workflows/build-lpcxpresso804.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-lpcxpresso804.yml)
[![Build LPCXpresso845-MAX](https://github.com/fastled/fbuild/actions/workflows/build-lpcxpresso845max.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-lpcxpresso845max.yml)

### Silicon Labs

[![Build MGM240](https://github.com/fastled/fbuild/actions/workflows/build-mgm240.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-mgm240.yml)
[![Build SparkFun Thing Plus Matter](https://github.com/fastled/fbuild/actions/workflows/build-thingplusmatter.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-thingplusmatter.yml)

### Raspberry Pi Pico

[![Build Raspberry Pi Pico](https://github.com/fastled/fbuild/actions/workflows/build-rpipico.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-rpipico.yml)
[![Build Raspberry Pi Pico 2](https://github.com/fastled/fbuild/actions/workflows/build-rpipico2.yml/badge.svg?branch=main)](https://github.com/fastled/fbuild/actions/workflows/build-rpipico2.yml)

Board descriptions and family deep-dives live in
[`docs/BOARD_STATUS.md`](docs/BOARD_STATUS.md).

## Build performance

[![Arduino Uno and ESP32-S3 Blink build benchmark](https://raw.githubusercontent.com/FastLED/fbuild/benchmark-stats/benchmark.svg?view=per-board-scale)](https://fastled.github.io/fbuild/)

The chart is regenerated nightly from clean-output (cold) and immediate repeat
(warm) Arduino Uno and ESP32-S3 Blink builds. Raw measurements are discoverable through the
[benchmark manifest](https://raw.githubusercontent.com/FastLED/fbuild/benchmark-stats/manifest.json).

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
| `fbuild install` | Download an environment's platform, toolchains, framework, tools and `lib_deps` without compiling (`--check` exits 2 when something is missing). |
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
- Host tools as Rust-built APE native executables: one binary per tool that
  runs everywhere fbuild does, even on NixOS
  ([details](#host-tools-ape-native-executables))
- Transparent architecture with Rust workspace internals documented under
  [`docs/architecture/`](docs/architecture/README.md)

See [`docs/WHY.md`](docs/WHY.md) for the full rationale, benefits, and
performance notes.

## Host Tools: APE Native Executables

The tools fbuild runs on the host for each platform are moving to FastLED-built
Rust executables shipped in a single format: the cosmocc
[Actually Portable Executable](https://justine.lol/ape.html) (APE). That covers
upload/flash utilities, deployers and image/format helpers that would otherwise
come as Python scripts, per-OS vendor binaries, or packages that behave
differently on each host. Each tool is built once and runs as a native
executable on every host fbuild supports: Linux, macOS and Windows, x86_64 and
aarch64. There is one artifact per tool instead of one per OS and CPU, and one
behavior to test instead of a matrix.

The goal per platform is to replace its third-party host tools with Rust tools
whose bugs FastLED fixes once and then ships as APE, so the fix reaches every
host at the same time.

**Reference platform: NXP LPC8xx.** Its flasher is FastLED's fork of the Rust
[`probe-rs`](https://probe.rs). The fork carries the fixes that make the
LPC845-BRK's LPC-Link2 CMSIS-DAP firmware work: spec-default fallbacks when
the firmware doesn't answer `DAP_Info`, an explicit `DAP_Connect(Swd)` with
retry, and a CMSIS-DAP v1 HID transport over `nusb` (FastLED/fbuild#935,
FastLED/fbuild#936). The source is on the `tools` branch of
[`FastLED/framework-arduino-lpc8xx`](https://github.com/FastLED/framework-arduino-lpc8xx),
and `fastled-release-cross.yml` cross-builds it today as **six** per-host
release assets: Windows, Linux and macOS, each on x86_64 and aarch64. fbuild
pins and checksums all six in `crates/fbuild-deploy/src/probe_rs.rs`. The APE
target is one asset, built and pinned once, that runs on all of those hosts.

**Next: WCH CH32V.** Its flashers, [`wlink`](https://github.com/ch32-rs/wlink)
(WCH-LinkE) and [`wchisp`](https://github.com/ch32-rs/wchisp) (USB ISP), are
already Rust. fbuild pins them per host in
`crates/fbuild-deploy/src/wlink.rs` and `wchisp.rs`, and `wlink` has assets for
only three hosts. Every other host needs `FBUILD_WLINK_PATH`. An APE build
removes that gap.

### Guaranteed launch, nothing to install

An APE tool launches anywhere fbuild does. Nothing has to be installed on the
host: no `ape` loader, no `binfmt_misc` registration, no shell support. That
guarantee is needed because most Unix hosts can't exec an APE file directly.
`posix_spawn` returns `ENOEXEC` ("Exec format error") unless an APE handler is
registered, and some distros, NixOS among them, ship none. Every fbuild spawn
detects the APE magic (`MZqFpD='`, `jartsr='`, `APEDBG='`) and runs the tool
through a loader:

| Host | How an APE tool runs |
|---|---|
| Windows | Natively; an APE file is a valid PE. |
| Linux | fbuild extracts the loader embedded in the tool itself, checks it is a 64-bit ELF for the host CPU, and caches it in an owner-only, exec-capable directory. If no such directory exists (read-only `HOME`, `noexec` `/tmp`), it runs the loader from a sealed in-memory file instead. The tool's environment needs no `PATH`, `sh`, coreutils, `HOME` or `TMPDIR`. |
| macOS | x86_64: fbuild extracts the tool's embedded loader and turns it into a Mach-O (the prologue's header move). Apple Silicon: fbuild compiles the tool's embedded `ape-m1.c` once with `/usr/bin/cc` (Xcode Command Line Tools). Both are cached like on Linux. |

Loader precedence is `FBUILD_APE_LOADER` (explicit override), then the loader
embedded in the tool, then `ape` on `PATH`, `/usr/bin/ape` and
`/usr/local/bin/ape`, then `/bin/sh`. Set `FBUILD_APE_CACHE_DIR` to choose
where extracted loaders are cached. Parallel builds and deploys are safe: fbuild
holds a fork lock while it writes a loader, so a concurrent first spawn can't
fail with `ETXTBSY` ("Text file busy").

### Adding an APE tool

Build the Rust tool with cosmocc so the output is an APE, provision it like any
other package, and spawn it through `fbuild_core::subprocess::run_command*`, or
build the command with `fbuild_core::platform::process::command` /
`tokio_command` instead of `Command::new`. The tool then works like any other
executable, with no per-host special casing. A hello-world fixture and its build
script live in
[`crates/fbuild-core/data/ape-hello/`](crates/fbuild-core/data/ape-hello/README.md).
Implementation notes are in
[`crates/fbuild-core/src/platform/README.md`](crates/fbuild-core/src/platform/README.md).

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

`fbuild` is free software licensed under the GNU Affero General Public
License v3.0 only (AGPL-3.0-only). See [LICENSE](LICENSE).

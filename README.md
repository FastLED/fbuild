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

## Installation

```bash
pip install fbuild
```

For source installs, platform notes, and first-run cache behavior, start with
the [getting started guide](docs/getting-started/README.md).

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

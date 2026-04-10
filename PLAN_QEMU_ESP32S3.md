# Plan: Non-Docker QEMU for ESP32-S3

## Short answer

Docker is not absolutely needed.

The strongest reason is Espressif's own QEMU documentation for ESP32-S3: they officially support QEMU for ESP32-S3, provide prebuilt binaries for Windows x64, Linux x64/arm64, and macOS x64/arm64, and document installation through `idf_tools.py`, not Docker.

FastLED's current implementation uses Docker as a packaging and reproducibility layer. It is not evidence that Docker is technically required for running ESP32-S3 firmware in QEMU.

## Decision

Implement this as `native-only` for supported hosts.

Rationale:

- Espressif already publishes official prebuilt QEMU binaries for the mainstream host matrix we care about.
- The native path matches Espressif's documented installation and execution flow.
- Docker adds another runtime layer, more packaging work, and more host-specific behavior than we need for the first production implementation.
- If a host is unsupported, fail explicitly rather than adding a second runtime path.

## Status

Implemented in the Rust codebase for ESP32-S3 on supported hosts:

- native QEMU tool discovery and managed install
- merged ESP32-S3 flash image generation
- local QEMU subprocess runner
- shared monitor and crash-decoder integration
- QEMU-specific UART0 build overrides and config validation
- `fbuild deploy --qemu` routing
- explicit unsupported-host failure
- unit coverage for tool resolution, image creation, command building, runner success, and crash decoding
- ignored end-to-end fixture coverage for the real ESP32-S3 build + QEMU path

## Top-level tasks

1. Implement native QEMU tool discovery and managed install for supported hosts.
2. Add ESP32-S3 flash-image generation for QEMU from existing build artifacts.
3. Add a local QEMU runner that launches `qemu-system-xtensa` and streams output.
4. Reuse the daemon monitor and crash-decoder pipeline for QEMU stdout.
5. Add QEMU-specific ESP32-S3 build adjustments for UART0 and config validation.
6. Wire `fbuild deploy --qemu` to the new native runner and remove the stale Docker wording.
7. Fail explicitly on unsupported hosts rather than adding a container fallback.
8. Add unit and integration coverage for tool resolution, image creation, boot success, and crash decoding.

## Important scope note

The issue you linked, `FastLED/fbuild#25`, was opened on April 10, 2026 and is explicitly scoped to `esp32dev` first. It also lists ESP32-S3 support as a non-goal for that first slice.

That said, your request is specifically about `esp32s3`, and this Rust repo is already better positioned for an ESP32-S3-first implementation than the issue text suggests:

- `crates/fbuild-build/src/esp32/orchestrator.rs` already emits `bootloader.bin`, `partitions.bin`, and `firmware.bin`.
- `crates/fbuild-deploy/src/esp32.rs` already codifies the ESP32-S3 flash offsets: bootloader `0x0`, partitions `0x8000`, app `0x10000`.
- `crates/fbuild-daemon/src/handlers/operations.rs` already has the timeout / halt-on-success / halt-on-error monitor loop.
- `crates/fbuild-serial/src/crash_decoder.rs` already decodes Xtensa crash output, including ESP32-S3.

## What I found

### In `fbuild2`

- `fbuild deploy --qemu` is now implemented natively in Rust.
- ESP32-S3 build output already contains the files QEMU needs.
- ESP32 boards already default to safe DIO flash mode in `crates/fbuild-config/src/board.rs` unless the user explicitly overrides it back to `qio`.
- The repo already warns about `ARDUINO_USB_CDC_ON_BOOT=1` because USB CDC can block / misroute serial output on ESP32-family boards.

### In `~/dev/fastled`

- The current QEMU path is hard-wired to Docker in `ci/docker_utils/qemu_test_integration.py` and `ci/docker_utils/qemu_esp32_docker.py`.
- The actual QEMU invocation is simple:
  - `qemu-system-xtensa`
  - `-machine esp32s3`
  - `-drive file=flash.bin,if=mtd,format=raw`
  - `-nographic`
  - `-serial mon:stdio`
  - watchdog disable via `-global driver=timer.esp32s3.timg,property=wdt_disable,value=true`
- FastLED also has one important ESP32-S3-specific build adjustment for QEMU:
  - force UART0 output
  - disable USB CDC / USB mode defines for emulated runs

### From Espressif

- Official ESP-IDF docs state that Espressif maintains a QEMU fork with ESP32-S3 support.
- Official docs also state that ESP-IDF provides prebuilt QEMU binaries for:
  - x64 Windows
  - x64 Linux
  - arm64 Linux
  - x64 macOS
  - arm64 macOS
- Official install path is:
  - `python $IDF_PATH/tools/idf_tools.py install qemu-xtensa`
- Official ESP32-S3 run flow is:
  - merge a full flash image
  - run `qemu-system-xtensa -nographic -machine esp32s3 -drive file=...,if=mtd,format=raw`

## Recommended implementation

I would implement this without Docker and keep Docker out of the first version entirely.

Native is the only supported product path for this feature.

### 1. Treat QEMU as a cached tool package

Add a small QEMU package abstraction under `fbuild-packages` instead of shelling out to Docker or requiring users to preinstall QEMU manually.

Preferred shape:

- Add an `EspQemuTool` package that resolves and installs Espressif's official QEMU archives into the existing `~/.fbuild/.../cache` tree.
- Resolve binaries from Espressif metadata rather than hardcoding a random download URL.
- For the ESP32-S3-first slice, only `qemu-xtensa` is strictly required.
- Keep `qemu-riscv32` in mind for later `esp32c3` support, but do not block ESP32-S3 on it.

Why this is the right fit:

- the repo already has a cache/download/extract system
- this keeps the feature reproducible across machines
- it avoids introducing an IDF checkout as a hard prerequisite

Fallback if metadata integration is slower than expected:

- first implementation may support `FBUILD_QEMU_XTENSA_PATH`
- then auto-discover from an existing ESP-IDF install
- then add the managed download path immediately after

I would not add a Docker fallback sequence.

### 2. Add an ESP32 flash-image merger for QEMU

Add a pure Rust helper, probably in `fbuild-deploy`, that:

- locates `bootloader.bin`, `partitions.bin`, and `firmware.bin`
- determines flash size from board config, with MCU default fallback
- creates a raw flash image filled with `0xFF`
- writes:
  - bootloader at `0x0`
  - partitions at `0x8000`
  - app at `0x10000`

For ESP32-S3, this is enough for the first slice unless testing proves an additional artifact is required. If we discover that a generated `flash_args`-style image is more reliable, I would switch the merger to follow the same artifact manifest model rather than special-casing more offsets ad hoc.

### 3. Add a real `QemuEsp32Runner`

Implement a local process runner that:

- resolves `qemu-system-xtensa`
- launches it with `-machine esp32s3`
- mounts the merged image with `-drive file=...,if=mtd,format=raw`
- uses `-nographic -serial mon:stdio -monitor none`
- disables watchdog resets using the same timer override FastLED already uses
- captures stdout/stderr line by line

This runner should be generic over machine type even if only `esp32s3` is enabled at first.

### 4. Reuse the existing daemon monitor logic instead of inventing a second monitor

Do not try to fake QEMU into `SharedSerialManager`.

That would create unnecessary complexity because QEMU is a child process, not a serial port.

Instead:

- refactor the current line-oriented monitor logic in `crates/fbuild-daemon/src/handlers/operations.rs` into a reusable helper that accepts a stream of lines
- feed QEMU stdout into that helper
- run the existing regex-based timeout / expect / halt-on-success / halt-on-error logic unchanged
- instantiate `CrashDecoder` with:
  - the built `firmware.elf`
  - the ESP32-S3 `addr2line` path derived from the build toolchain
- whenever QEMU output contains a crash dump, inject decoded stack lines into the same monitor stream

This gives us the behavior we want while keeping serial and emulator paths separate.

### 5. Make ESP32-S3 QEMU builds force UART0-friendly defines

This is the main behavioral gotcha for ESP32-S3.

QEMU exposes UART0. Many ESP32-S3 board definitions route `Serial` to USB CDC instead.

For QEMU-mode builds I would append user-level overrides equivalent to:

```text
-DARDUINO_USB_MODE=0
-DARDUINO_USB_CDC_ON_BOOT=0
```

I would not mutate board JSONs. I would apply these only for QEMU-mode builds so real hardware behavior stays unchanged.

I would also validate that the effective flash mode remains DIO-compatible. Since `fbuild2` already defaults ESP32-family boards to DIO, this is mostly a guard against explicit user overrides such as `board_build.flash_mode = qio`.

### 6. Land the first CLI surface on `deploy --qemu`

For this repo, the shortest correct path is to wire the feature into the existing deploy flow:

- keep `fbuild deploy --qemu`
- remove the "requires Docker" wording
- build if needed
- create merged flash image
- run QEMU locally
- stream output through the existing monitor/crash-decode path

I would not start by adding a brand-new `test-emu` subcommand.

Reason:

- there is no native `test` command in this Rust CLI today
- `deploy` already owns post-build execution semantics
- adding a new top-level command before the engine exists is interface churn

If issue parity later requires `fbuild test-emu`, it can be a thin wrapper over the same runner.

## Implementation instructions

### Native toolchain policy

Implement tool resolution in this order:

1. Respect `FBUILD_QEMU_XTENSA_PATH` if set.
2. Check a managed `fbuild` cache install of Espressif QEMU.
3. Auto-discover an existing ESP-IDF tools install if present.
4. Offer or trigger a managed native install on supported hosts.
5. Fail explicitly if the host is unsupported or native setup cannot be completed.

Supported native host matrix for first release:

- Windows x64
- Linux x64
- Linux arm64
- macOS x64
- macOS arm64

If the host is outside that matrix, fail explicitly.

### Native install behavior

- Do not require a full ESP-IDF checkout just to get QEMU.
- Prefer downloading the official Espressif QEMU archives directly into the `fbuild` cache.
- Keep the extracted bundle intact; on Windows this avoids breaking DLL adjacency assumptions.
- Record the resolved executable path in logs when `--verbose` is enabled.

### Flash image generation behavior

Build a merged raw flash image locally before launch.

For ESP32-S3:

- use `bootloader.bin` at `0x0`
- use `partitions.bin` at `0x8000`
- use `firmware.bin` at `0x10000`
- fill unused regions with `0xFF`
- support only QEMU-valid flash sizes: `2MB`, `4MB`, `8MB`, or `16MB`

Prefer implementing the merger in Rust. If later testing shows that `flash_args` is required for correctness, switch the merger backend without changing the runner API.

### QEMU launch behavior

Base command for the first release:

```text
qemu-system-xtensa
-nographic
-machine esp32s3
-drive file=<flash_image>,if=mtd,format=raw
-serial mon:stdio
-monitor none
-global driver=timer.esp32c3.timg,property=wdt_disable,value=true
```

Notes:

- Keep the watchdog disable override because it matches Espressif's current ESP32-S3 guidance.
- `timer.esp32c3.timg` is the correct property target for ESP32-S3 at the moment.
- Add optional PSRAM, eFuse, and networking flags later; do not block the first slice on them.

### Build-mode behavior

When `--qemu` targets ESP32-S3:

- inject `-DARDUINO_USB_MODE=0`
- inject `-DARDUINO_USB_CDC_ON_BOOT=0`
- reject explicitly unsafe flash configurations
- surface a clear error when the selected board or MCU is not supported by the first implementation

Do not mutate board JSON definitions. These are emulation-only overrides applied at build time.

### Monitor integration behavior

- Treat QEMU as a subprocess, not a serial device.
- Refactor the existing monitor loop so it can consume generic line streams.
- Feed QEMU stdout into that same path.
- Attach `CrashDecoder` using the produced `firmware.elf` and derived `addr2line`.
- Keep timeout, expect, halt-on-success, and halt-on-error semantics identical between physical and emulated runs.

### Unsupported hosts policy

Docker is not part of this implementation.

If the host architecture is outside Espressif's published native binary matrix, or if native QEMU packaging is broken on a given host, fail explicitly with a clear error.

## Concrete implementation sequence

### Phase 1: minimal engine

1. Add local QEMU tool resolution and installation.
2. Add flash-image merge support for ESP32-S3.
3. Add local QEMU command builder and process runner.
4. Wire `deploy --qemu` to use the runner instead of erroring out.

### Phase 2: monitor and crash integration

1. Refactor daemon monitor loop into a generic line consumer.
2. Feed QEMU process output into it.
3. Hook in `CrashDecoder` using `firmware.elf` and derived `addr2line`.

### Phase 3: ESP32-S3 QEMU build-mode adjustments

1. Force UART0-friendly build defines for QEMU-mode builds.
2. Validate flash mode / flash size compatibility before launch.
3. Emit explicit failure messages for unsupported or unsafe configs.

### Phase 4: tests and fixtures

1. Unit tests for ESP32-S3 flash merge offsets and flash size handling.
2. Unit tests for QEMU command generation.
3. Unit tests for tool discovery and path resolution.
4. Integration fixture for a success case:
   - build `tests/platform/esp32s3`
   - run in QEMU
   - expect `Hello from ESP32-S3!`
5. Integration fixture for a crash case:
   - add a tiny sketch that intentionally calls `abort()` or dereferences null
   - verify crash classification and decoded stack output

## Acceptance criteria I would use

- `fbuild deploy -e esp32s3 --qemu --monitor --timeout 15` runs locally without Docker.
- First run auto-installs or clearly locates local QEMU.
- ESP32-S3 builds produce a valid merged flash image and boot in QEMU.
- Serial output is visible in the CLI without requiring USB CDC.
- Crash output is classified and decoded through the existing Rust crash decoder path.
- If the board is not supported, or if the config is not QEMU-safe, the error is explicit.

## Risks

### Risk 1: USB CDC vs UART0

This is the highest-probability failure point for ESP32-S3.

Mitigation:

- always force QEMU-mode builds to UART0-friendly macros
- test that change with the existing `tests/platform/esp32s3` fixture

### Risk 2: flash image layout mismatches

If a simple three-artifact merge is insufficient, boot will fail silently or reset-loop.

Mitigation:

- keep the merger isolated
- if needed, move to a `flash_args`-driven merger without changing the runner API

### Risk 3: Windows packaging details

The repo is being developed on Windows here, and QEMU may require DLLs next to the executable depending on how Espressif ships the archive.

Mitigation:

- validate the extracted layout once
- make tool resolution return the executable inside the extracted bundle root
- keep the whole extracted tree intact in cache

## Unsupported host stance

If native QEMU cannot be supported reliably on a host, that host is unsupported until the native path is fixed.

## Recommendation

Proceed with a non-Docker ESP32-S3 implementation.

I would build it as:

- cached official Espressif QEMU tool install
- Rust flash-image merger
- local QEMU process runner
- reuse of existing daemon monitor and crash decoder
- QEMU-specific UART0 build overrides for ESP32-S3

## Sources

- Issue: https://github.com/FastLED/fbuild/issues/25
- Espressif QEMU docs for ESP32-S3: https://docs.espressif.com/projects/esp-idf/en/stable/esp32s3/api-guides/tools/qemu.html
- FastLED Docker-based QEMU runner reference:
  - `~/dev/fastled/ci/docker_utils/qemu_esp32_docker.py`
  - `~/dev/fastled/ci/docker_utils/qemu_test_integration.py`
  - `~/dev/fastled/ci/runners/qemu_runner.py`

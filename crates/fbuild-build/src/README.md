# Source

Build orchestration for all supported embedded platforms.

## Top-Level Modules

- **`lib.rs`** -- `PlatformSupport` and `BuildOrchestrator` traits, `BuildParams`/`BuildResult` types
- **`compiler.rs`** -- `Compiler` trait and `CompilerBase` shared logic (flags, gcc/g++ invocation, rebuild detection)
- **`linker.rs`** -- `Linker` trait and `LinkerBase` shared logic (ar, objcopy, size reporting)
- **`parallel.rs`** -- Multi-threaded source compilation using `std::thread::scope` with work-stealing
- **`pipeline.rs`** -- Shared build pipeline (config parse, board load, build dir setup, compile, link)
- **`source_scanner.rs`** -- Finds .cpp/.cc/.cxx/.c/.S/.ino files; preprocesses .ino into valid .cpp
- **`compile_database.rs`** -- Generates `compile_commands.json` for clangd/IDE support
- **`build_output.rs`** -- Uniform build log formatting across all platforms
- **`zccache.rs`** -- Optional zccache compiler cache wrapper integration
- **`compile_many.rs`** -- Two-stage primitive for batched sketch builds (FastLED/fbuild#238): framework + libs built once with `--framework-jobs`, then per-sketch compile + link fanned out across `--sketch-jobs` workers

## Native `extra_scripts` Boundary

The native replay path intentionally supports a narrow subset of PlatformIO script behavior:

- Supported script prefixes: `pre:` and `post:`
- Supported imports: `Import("env")`; `Import("projenv")` only in post scripts
- Supported mutation scopes: `CPPDEFINES`, `CPPPATH`, `CCFLAGS`, `CFLAGS`, `CXXFLAGS`, `ASFLAGS`, `LINKFLAGS`, `LIBPATH`, `LIBS`
- Supported no-op helpers: `AddPreAction`, `AddPostAction`, `AlwaysBuild`, `Alias`, `Depends`

Anything outside that matrix fails fast with a `use --platformio` recommendation so unsupported scripts do not silently produce a partial build.

## Platform Subdirectories

- **`avr/`** -- AVR ATmega (Arduino Uno, Mega, Nano)
- **`esp32/`** -- ESP32 family (ESP32, S3, C3, C6, etc.)
- **`esp8266/`** -- ESP8266 (NodeMCU, Wemos D1)
- **`teensy/`** -- Teensy 4.x (ARM Cortex-M7)

# `platformio.ini` Reference

fbuild reads PlatformIO-style project configuration. The minimum project has a
`platformio.ini` and a `src/` directory.

## Minimal Configuration

```ini
[env:uno]
platform = atmelavr
board = uno
framework = arduino
```

Build it with:

```bash
fbuild build -e uno
```

## Common Configuration

```ini
[platformio]
default_envs = uno

[env:uno]
platform = atmelavr
board = uno
framework = arduino
upload_port = COM3
monitor_speed = 9600
monitor_filters = default, esp32_exception_decoder
check_tool = clangtidy
build_flags =
    -DDEBUG
    -DLED_PIN=13
lib_deps =
    https://github.com/FastLED/FastLED
    https://github.com/adafruit/Adafruit_NeoPixel
```

Common keys:

| Key | Purpose |
|---|---|
| `[platformio] default_envs` | Default environment when `-e` is omitted. |
| `[env:<name>] platform` | Platform or platform package URL. |
| `[env:<name>] board` | Board id. See [BOARD_STATUS.md](../BOARD_STATUS.md). |
| `[env:<name>] framework` | Framework, usually `arduino`. |
| `upload_port` | Preferred deploy port. |
| `monitor_speed` | Serial monitor baud rate. |
| `monitor_filters` | Serial monitor filters. ESP32-family boards default to `default, esp32_exception_decoder` when unset; use `monitor_filters = []` to suppress. |
| `check_tool` | Static-analysis tool for PlatformIO `pio check` compatibility; ignored by normal fbuild compilation. |
| `build_flags` | Extra compiler flags. |
| `lib_deps` | Library dependencies, including GitHub URLs. |
| `build_type` | Build profile; `debug` preserves unwind metadata. |
| `board_build.*` | Board-specific build overrides. |
| `board_upload.*` | Upload/deploy overrides. |

The config parser also supports environment inheritance and variable
substitution. Architecture notes for the parser live in
[`docs/architecture/overview.md`](../architecture/overview.md).

## Library Dependencies

fbuild can download and compile Arduino libraries directly from GitHub URLs:

```ini
[env:uno]
platform = atmelavr
board = uno
framework = arduino
lib_deps =
    https://github.com/FastLED/FastLED
```

Supported behavior includes:

- GitHub URL optimization to zip downloads.
- Branch detection for common default branches.
- Arduino library layout handling.
- LDF-style transitive header scanning for library selection.
- LTO-aware library builds for smaller firmware.

The library selection design is documented in
[`docs/architecture/library-selection.md`](../architecture/library-selection.md).

## ESP QEMU Flash Mode

ESP32-family QEMU requires DIO flash mode:

```ini
[env:esp32s3]
platform = https://github.com/pioarduino/platform-espressif32/releases/download/55.03.34/platform-espressif32.zip
board = esp32-s3-devkitc-1
framework = arduino
board_build.flash_mode = dio
board_upload.flash_mode = dio
```

See [emulator testing](../guides/emulator-testing.md) for backend-specific
requirements.

## ESP `sdkconfig`

ESP-IDF `CONFIG_*` override design is documented in
[`docs/sdkconfig.md`](../sdkconfig.md). Prefer `sdkconfig.fragment` for larger
config changes; use `build_flags = -D CONFIG_*` for simple PlatformIO-compatible
overrides.

## Native `extra_scripts`

Native fbuild interprets `extra_scripts` against a mock SCons `env` — it does not
run real SCons. It covers the common flag/path mutations (`Append`/`Replace`/etc.
over `CPPDEFINES`, `CCFLAGS`, `CXXFLAGS`, `LINKFLAGS`, `LIBS`, …), `BUILD_FLAGS`,
`DefaultEnvironment()`, and project introspection (`GetBuildType`,
`GetProjectOptions`). Non-flag tool scopes are recorded as notes; genuinely
unsupported behavior fails early with a recommendation to use `--platformio`.
See [`crates/fbuild-build/README.md`](../../crates/fbuild-build/README.md) for the
full supported/rejected list and the structural limitations of the mock.

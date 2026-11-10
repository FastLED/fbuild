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

### Merged: `[platformio] extra_configs`

PlatformIO can extend the base file with
[`extra_configs`](https://docs.platformio.org/en/latest/projectconf/sections/platformio/options/generic/extra_configs.html),
and fbuild honours it the same way:

```ini
[platformio]
extra_configs =
  extra_envs.ini          ; environments shared by several projects
  ci/*.ini                ; shell-style wildcards are allowed
```

- Paths and wildcards resolve against the directory holding `platformio.ini`
  (not the current directory). Wildcard matches are applied in sorted order so
  "later wins" is deterministic.
- The extra files are parsed with the same syntax as `platformio.ini` and merged
  over it: an option declared in both places takes the extra file's value, later
  files win over earlier ones, and `[env:*]`, `[env]` and custom groups such as
  `[common]` may be declared in either place. Base `[env]` data still applies to
  environments introduced by an extra file, and `${...}` substitution runs after
  the merge, so `${common.build_flags}` sees the merged value.

Three cases are refused rather than approximated, because PlatformIO would
either fail or silently drop the declaration:

| Case | Why it is refused |
|---|---|
| A listed file does not exist, or a wildcard matches no *file* | PlatformIO would not build this project either. |
| An extra file declares `extra_configs` again | PlatformIO merges the option but never processes it recursively, so the nested files would never be read. |
| The list resolves back to `platformio.ini` itself | Merging a file into itself is a mistake, not a configuration. |

An empty value (or one that is only a comment) declares nothing and is
accepted. A same-named key inside `[env:*]` is not a file include in
PlatformIO either, so it is not treated as one here.

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

## ESP32 Platform Pins

For ESP32 environments fbuild honors a pioarduino release archive as the
`platform` value, as PlatformIO does, and builds against the framework that
release names (FastLED/fbuild#1432):

```ini
platform = https://github.com/pioarduino/platform-espressif32/releases/download/55.03.35/platform-espressif32.zip
```

A `platform_packages = platform-espressif32@<URL>` entry takes precedence. A
bare `espressif32` uses the pioarduino `stable` release. Registry pins
(`espressif32@6.5.0`) and git URLs cannot be fetched; fbuild logs a warning and
uses `stable` instead (FastLED/fbuild#1407).

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

## ESP-IDF (CMake) component builds

By default fbuild builds ESP32 environments with the Arduino core and its
precompiled ESP-IDF libraries, so a genuine `idf_component_register()` project
is rejected (FastLED/fbuild#1664). Opt in to the delegated build to have the
project's own `CMakeLists.txt` built by fbuild's pinned ESP-IDF release:

```ini
[env:esp32s3]
platform = espressif32
board = esp32-s3-devkitc-1
framework = espidf
board_build.build_system = espidf
```

- `native` (default) keeps the Arduino path unchanged.
- `espidf` provisions the pinned ESP-IDF release (source, toolchains, CMake,
  Ninja, Python environment — ~1.8 GB on first use), runs
  `idf.py set-target <chip>` and `idf.py build`, and publishes
  `bin`/`elf`/`map`/`bootloader.bin`/`partition-table.bin`/`sdkconfig`/
  `compile_commands.json` under `<build>/idf/`, with an `idf-artifacts.json`
  index recording each file's byte count.
- Only the `esp32s3` target is provisioned today; another MCU is a
  configuration error naming the chip.

A failing or timed-out delegated build reports its stage, argv, exit code and
the tail of both streams.

## Native `extra_scripts`

Native fbuild interprets `extra_scripts` against a mock SCons `env` — it does not
run real SCons. It covers the common flag/path mutations (`Append`/`Replace`/etc.
over `CPPDEFINES`, `CCFLAGS`, `CXXFLAGS`, `LINKFLAGS`, `LIBS`, …), `BUILD_FLAGS`,
`DefaultEnvironment()`, and project introspection (`GetBuildType`,
`GetProjectOptions`). Non-flag tool scopes are recorded as notes; genuinely
unsupported behavior fails early with a recommendation to use `--platformio`.
See [`crates/fbuild-build/README.md`](../../crates/fbuild-build/README.md) for the
full supported/rejected list and the structural limitations of the mock.

# fbuild-config

PlatformIO INI parser, board configuration, and MCU memory specifications.

## Key Types

- `PlatformIOConfig` -- Parses `platformio.ini` with `extends` inheritance, `${section.key}` variable substitution, and multi-line values
- `BoardConfig` -- Board definition loaded from built-in JSON assets or `boards.txt`; provides MCU, clock, variant, upload, and flash/RAM fields
- `McuSpec` -- MCU memory limits (max flash, max RAM) used for size validation

## Modules

- **ini_parser** -- INI parser with section inheritance, variable substitution, inline comments, and base `[env]` merging
- **board** -- Board configuration loading from 1,610 built-in JSON definitions and platformio.ini overrides
- **mcu** -- MCU flash and RAM size specifications

## Binary Targets

- **enrich_boards** -- Maintenance tool that enriches stripped board JSONs with `build`/`upload` sections from local PlatformIO platform installs

# Source

## Modules

- **`lib.rs`** -- Crate root; re-exports `PlatformIOConfig`, `BoardConfig`, `McuSpec`
- **`ini_parser.rs`** -- PlatformIO INI parser with `extends` inheritance and `${section.key}` variable substitution
- **`board.rs`** -- Board configuration from built-in JSON assets, boards.txt, and platformio.ini overrides
- **`mcu.rs`** -- `McuSpec` struct defining MCU flash and RAM limits

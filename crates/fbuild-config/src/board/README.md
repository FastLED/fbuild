# board module

Board configuration loaded from Arduino `boards.txt` files or the embedded
PlatformIO board JSON database. Re-exported from the parent crate as
`fbuild_config::BoardConfig`, `DebugToolMeta`, and `Esp32QemuPsramConfig`.

## Files

- `mod.rs` — Module root and public re-exports.
- `types.rs` — `BoardConfig`, `DebugToolMeta`, `Esp32QemuPsramConfig`, and the
  internal `EMULATOR_TOOL_NAMES` list.
- `loaders.rs` — `BoardConfig::from_boards_txt`, `BoardConfig::from_board_id`,
  and the shared `parse_boards_txt` line parser.
- `methods.rs` — Accessor / derivation methods: `emulators`, `has_emulator`,
  `effective_esp32_memory_type`, `qemu_esp32_psram_config`, `platform`,
  `get_defines`, `get_include_paths`.
- `db.rs` — Embedded JSON board database (`include_dir!`), board-id alias
  resolution, debug-tool extraction, and the flat defaults map consumed by
  `from_board_id`.
- `tests.rs` — Unit tests covering loaders, defaults, defines, ESP32 flash /
  PSRAM heuristics, and database invariants.

This split was introduced to satisfy the CI LOC gate (no `.rs` file may
exceed 1000 lines); behaviour and the public API are unchanged.

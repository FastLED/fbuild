# symbol_analysis

Pure parsers and aggregators behind `fbuild bloat` (legacy `fbuild symbols`):

- `parse_nm_line` / `parse_nm_output` — `nm --print-size -S` row parsing.
- `parse_linker_map` — GNU `ld -Map` output → per-input-section ranges.
- `classify_region` — nm type letter → `Flash` / `Ram` bucket.
- `FineGrainedSymbolMap::retain_loaded_symbols` — drop symbols whose `[addr, addr+size)` doesn't fit any `PT_LOAD` region, so linker-script boundary markers (`__StackTop`, `__flash_arduino_end`) don't pollute the bloat report.
- `build_fine_grained_map_with_synth` — fold nm rows + map ranges + demangled names into the per-symbol report.

Intentionally has no ELF-parsing dep; ELF I/O lives in `fbuild_build::symbol_analyzer`, which calls into this module.

## Files

- `mod.rs` — types and pure functions.
- `tests.rs` — unit tests.

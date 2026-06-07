# symbol_analysis

Pure parsers and aggregators behind `fbuild bloat` (legacy `fbuild symbols`):

- `parse_nm_line` / `parse_nm_output` — `nm --print-size -S` row parsing.
- `parse_linker_map` — GNU `ld -Map` output → per-input-section ranges.
- `parse_cref_table` — GNU `ld --cref` `Cross Reference Table` block → mangled-symbol → referencer `(archive, object)` list. See [#459](https://github.com/FastLED/fbuild/issues/459). Empty result when the map lacks a cref block (older `ld`, `-Wl,--no-cref`) — never a hard error.
- `graph::BackrefGraph` — back-reference graph + Graphviz `.dot` rendering rooted at a target symbol. Consumes the cref `referenced_by` data, applies adaptive depth termination + per-node fan-out caps + collapse-archive rules so dense hubs (`printf` has ~25 mbedTLS referencers on ESP32-P4) stay readable. See [#463](https://github.com/FastLED/fbuild/issues/463).
- `classify_region` — nm type letter → `Flash` / `Ram` bucket.
- `FineGrainedSymbolMap::retain_loaded_symbols` — drop symbols whose `[addr, addr+size)` doesn't fit any `PT_LOAD` region, so linker-script boundary markers (`__StackTop`, `__flash_arduino_end`) don't pollute the bloat report.
- `build_fine_grained_map_with_synth` — fold nm rows + map ranges + demangled names + cref into the per-symbol report.

Each `FineGrainedSymbol` row carries a `referenced_by: Vec<SymbolReference>` field populated from the cref table. Granularity is `(archive, object)`, not per-symbol — that's a property of `ld --cref` itself.

Intentionally has no ELF-parsing dep; ELF I/O lives in `fbuild_build::symbol_analyzer`, which calls into this module.

## Files

- `mod.rs` — types and pure functions.
- `cref.rs` — `Cross Reference Table` parser.
- `graph.rs` — back-reference graph walker + `.dot` renderer.
- `tests.rs` — unit tests.

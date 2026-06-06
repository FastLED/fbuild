# `fbuild bloat` — does it report only loaded symbols?

## Verdict

**No.** The bloat report includes linker-script boundary symbols (e.g. `__StackTop`, `__flash_arduino_end`) that are not real bytes in the final binary. nm assigns them garbage "sizes" computed by subtracting from the next symbol's address, which inflates `total_flash` by **multiple gigabytes** and pollutes the top-N output.

## What the tool does

`fbuild symbols <elf>` (the current subcommand name; `bloat` rename in #443 only updated docs, the clap variant is still `Symbols`) runs:

```
nm --print-size --size-sort --reverse-sort -S <elf>
```

then routes the rows through `fbuild_core::symbol_analysis::build_fine_grained_map_with_synth`, which keeps only symbols whose nm type letter is `T t R r W w D d B b` via `classify_region` (`crates/fbuild-core/src/symbol_analysis/mod.rs:398-404`). Everything else (`A`, `U`, `N`, `?`, `C`, `V`, etc.) is dropped.

That filter is necessary but **not sufficient**.

## Empirical test: nrf52840 firmware

ELF: `tests/platform/nrf52840_dk/.fbuild/build/nrf52840_dk/quick/firmware.elf` (71 KB on disk; real `.text` is 32,740 bytes per `readelf -SW`).

Counts:

| Source                                | Symbols |
|---|---|
| `nm <elf>` (all)                      | 540 |
| `nm --print-size --size-sort -S <elf>` | 520 |
| `fbuild symbols <elf>` reported       | **520** |

The 20-symbol drop comes from nm's own filtering (size-0 markers and `A`-type absolute linker symbols). After that, `classify_region` doesn't reject any more rows — every surviving row is `T/t/R/r/W/w/D/d/B/b`, so all 520 make it into the report.

### The leak: linker boundary symbols

```
$ nm --print-size --size-sort -S firmware.elf | grep -E "__flash_arduino_end|__StackTop|__HeapBase"
20007ff8 00037808 B __HeapBase
20040000 dffedfe4 T __StackTop
000ed000 fff40fe4 T __flash_arduino_end
```

These are `PROVIDE(...)` statements in the linker script — address markers, not allocated bytes. nm computes their "size" from the gap to the next symbol, which produces:

- `__flash_arduino_end`: 4,294,184,932 bytes (~4 GB)
- `__StackTop`: 3,758,022,628 bytes (~3.5 GB)
- `__HeapBase`: 227,336 bytes (the actual .heap region — borderline; .heap is NOBITS, so not in the flash payload either, but at least the number reflects allocated address space).

Split:

| Bucket                              | Count | Σ flash      | Σ ram   |
|---|---|---|---|
| Sane (size < 100,000)               | 517   | 32,965 B     | 8,166 B |
| Bogus boundary symbols              | 3     | 8,052,207,560 B | 227,336 B |

32,965 B aligns with `readelf -SW`'s real `.text` size (0x07fe4 = 32,740 B; the small overshoot is the few weak-aliased symbols counted twice). The 8 GB bogus number is the inflated total fbuild prints:

```
Wrote 520 symbols to bloat_nrf52.json (flash=8052240525 B, ram=235502 B)
```

A user reading the top-N output sees `__flash_arduino_end` and `__StackTop` as the top "functions" by size — totally misleading.

## Root cause

Two compounding issues:

1. **`classify_region` is type-letter-only**, with no cross-check against the linker map's input-section ranges. A symbol with `output_section: null` (no map attribution) is still kept and counted.
2. **No upper-bound sanity check.** A symbol whose reported size exceeds the largest PT_LOAD segment (here 32,748 B for flash, 235,520 B for RAM) is structurally impossible but currently passes through.

## What a fix should do

Order of cheapness:

1. **Cheap (1-line fix in `build_fine_grained_map_with_synth`)**: drop any symbol whose `(addr + size)` extends past the end of the containing input-section range — or, lacking a range attribution (`InputSectionIndex::lookup` returns `None`) and size > 64 KB, drop it. The data is already collected; just gate the `symbols.push(...)`.
2. **Robust (recommended)**: pass through PT_LOAD segment bounds (parse with the `object` crate, already a workspace dep) and clamp/reject anything whose `addr+size` straddles a segment boundary. Use `readelf -lW` semantics, not just `-SW`.
3. **Belt-and-braces**: also subtract NOBITS sections from `total_flash` (so .bss/.heap symbols only count toward RAM, not flash payload).

Affected files:
- `crates/fbuild-core/src/symbol_analysis/mod.rs:540-567` (the main fold loop)
- `crates/fbuild-core/src/symbol_analysis/mod.rs:398-404` (`classify_region`)
- Tests in `crates/fbuild-core/src/symbol_analysis/tests.rs` should add a fixture row simulating `__StackTop`-style bogus size and assert it's filtered.

## Side observation

`fbuild symbols <project_dir>` is broken — it passes the directory to nm verbatim:

```
nm: Warning: 'C:/.../tests/platform/nrf52840_dk' is a directory
```

The ELF auto-discovery path appears unimplemented (or regressed). Worth its own ticket; not blocking the bloat filter fix.

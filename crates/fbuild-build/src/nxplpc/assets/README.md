# NXP LPC8xx linker / startup assets

| File              | Purpose                                                    |
| ----------------- | ---------------------------------------------------------- |
| `lpc804.ld`       | GNU ld memory map for LPC804 (32 KB Flash / 4 KB RAM).     |
| `lpc845.ld`       | GNU ld memory map for LPC845 (64 KB Flash / 16 KB RAM).    |
| `startup_lpc804.S`| Reset_Handler + minimal vector table for LPC804.           |
| `startup_lpc845.S`| Reset_Handler + minimal vector table for LPC845.           |

All four files are embedded into the `fbuild-build` crate via `include_str!`
in `mod.rs` so the Stage-2 orchestrator (FastLED/FastLED#2836) can emit them
into the build directory at link time without an extra package download.

Memory regions are taken from the NXP datasheets — verified against the
datasheets cited in each file header. The IRQ vector tables are intentionally
the ARMv6-M system-vector minimum; Stage 2 will expand to cover peripheral
IRQs as drivers need them.

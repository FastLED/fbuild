# NXP LPC8xx (Cortex-M0+) Build Support

Bare-metal CMSIS support for NXP LPC804 and LPC845 microcontrollers.

## Stage 1 / Stage 2

This module is Stage 1 of FastLED/FastLED#2836. Stage 1 adds:

- Board definitions (`lpc804`, `lpc845`)
- `Platform::NxpLpc` enum entry, `nxplpc` platform string parsing
- Cortex-M0+ MCU config JSON shared by both targets
- Linker scripts (`assets/lpc804.ld`, `assets/lpc845.ld`)
- Startup / vector table skeletons (`assets/startup_lpc804.S`, `assets/startup_lpc845.S`)
- `PlatformSupport` shim wired to the ARM GCC toolchain installer

Stage 2 (FastLED-side C++ port — separate repo, separate PR) will provide the
actual clockless / SPI driver glue. Until that lands, the build orchestrator
returns a clear "Stage 2 not landed" error and the per-platform CI workflows
will fail at the link step.

## Memory map (from NXP datasheets)

| Part   | Flash origin | Flash size | RAM origin   | RAM size |
| ------ | ------------ | ---------- | ------------ | -------- |
| LPC804 | 0x00000000   | 32 KB      | 0x10000000   | 4 KB     |
| LPC845 | 0x00000000   | 64 KB      | 0x10000000   | 16 KB    |

References:

- LPC804 datasheet: <https://www.nxp.com/docs/en/nxp/data-sheets/LPC804_DS.pdf>
- LPC84x datasheet: <https://www.nxp.com/docs/en/data-sheet/LPC84x.pdf>

## Upload

Primary path: ISP-via-UART with the on-die boot ROM, driven by `lpc21isp`.
Alternative: SWD via CMSIS-DAP / J-Link / pyOCD (entries in the board JSON
`debug.tools` map).

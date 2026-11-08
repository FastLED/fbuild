# SAM MCU Configs

Data-driven configuration for Atmel/Microchip SAM boards, embedded at compile time via `include_str!()`.

## Files

- **`sam3x.json`** -- Flags for SAM3X8E (Cortex-M3, Arduino Due)
- **`samd21.json`** -- Flags for SAMD21 (Cortex-M0+)
- **`samd51.json`** -- Flags for SAMD51 (Cortex-M4F)
- **`same5x.json`** -- Flags for SAME51/SAME53/SAME54 (Cortex-M4F)

## Entry symbols

Atmel's classic-SAM `variants/*/linker_scripts/gcc/flash.ld` declares no
`ENTRY()`, so `sam3x.json` supplies one on the link line
(`-Wl,--entry=Reset_Handler`) and marks it required
(`-Wl,--undefined=Reset_Handler`) — Arduino's own `platform.txt` does the same.
Without it nothing roots the startup object, `libsam`'s `Reset_Handler` is
never extracted from the archive, and `--gc-sections` silently discards the
whole application: the link exits 0 and reports a ~5 KB image containing no
`main`, no `setup` and no `loop` (FastLED/fbuild#1939).

`required_link_symbols` is checked against the linked ELF's symbol table, so a
link that produces such an inert image fails the build instead of being
reported as success. Adafruit's SAMD/SAME5x scripts declare `ENTRY()` in the
script itself and therefore declare no required symbols.

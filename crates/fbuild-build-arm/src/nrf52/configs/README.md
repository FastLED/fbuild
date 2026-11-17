# NRF52 MCU Configs

Data-driven configuration for Nordic NRF52 boards (ARM Cortex-M4F), embedded at compile time via `include_str!()`.

## Files

- **`nrf52840.json`** -- Flags for NRF52840 (Cortex-M4F)

## No LTO

No profile enables `-flto`. The Adafruit nRF52 core's own `platform.txt`
does not use it, and its FreeRTOS port (`port.c`) loads symbol addresses
with `ldr rN, =symbol` in naked functions, which fails at link time with
`offset out of range` once LTO emits the program as one unit
(FastLED/fbuild#2016). `mcu_config::tests::no_profile_enables_lto_by_default`
guards this.

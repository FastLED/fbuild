# Nucleo L476RG Test Fixture

ARM Cortex-M4F build validation project for the ST Nucleo L476RG (STM32L476RGT6).
Uses the Arduino framework on the `ststm32` platform.

## Status

**Currently fails to build** due to a known fbuild-owned gap: `stm32f4`
prefix-based config selection in `crates/fbuild-build/src/stm32/mcu_config.rs`
does not match STM32L4 MCUs (`stm32l4*`). The fixture exists as a regression
marker so that once an `stm32l4.json` Cortex-M4F config (no DSP) is added, the
build can be smoke-tested.

Tracks coverage for FastLED/FastLED#975 (FastLED on STM32L4) via fbuild
tracker FastLED/fbuild#385.

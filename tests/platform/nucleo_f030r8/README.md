# Nucleo F030R8 Test Fixture

ARM Cortex-M0 build validation project for the ST Nucleo F030R8 (STM32F030R8T6).
Uses the Arduino framework on the `ststm32` platform.

## Status

**Currently fails to build** due to a known fbuild-owned gap: `crates/fbuild-build/src/stm32/mcu_config.rs`
does not match STM32F0 MCUs (`stm32f0*`). Cortex-M0 needs its own `stm32f0.json`
config (no FPU, `-mcpu=cortex-m0`). The fixture exists as a regression marker so
that once F0 support is added the smoke build can be enabled in CI.

Tracks coverage for FastLED/FastLED#750 (FastLED on STM32F030C8T6) via fbuild
tracker FastLED/fbuild#385. STM32F030C8 and STM32F030R8 share the F030x8 die
and the same fbuild MCU config family will cover both.

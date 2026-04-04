# Generic ARM

Generic ARM Cortex-M build support shared across STM32, RP2040, NRF52, SAM, etc.

Provides `ArmCompiler`, `ArmLinker`, and `ArmMcuConfig` that platform-specific
orchestrators (STM32, RP2040, etc.) compose with their own framework and MCU configs.

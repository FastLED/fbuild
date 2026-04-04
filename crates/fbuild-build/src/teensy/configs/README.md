# Teensy MCU Configs

Data-driven configuration for Teensy boards (ARM Cortex-M), embedded at compile time via `include_str!()`.

## Files

- **`teensy4x.json`** -- Flags for Teensy 4.0 and 4.1 (Cortex-M7, IMXRT1062)
- **`teensy3x.json`** -- Flags for Teensy 3.5/3.6 (Cortex-M4 with FPU)
- **`teensy31.json`** -- Flags for Teensy 3.1/3.2 (MK20DX256, F_BUS=36MHz)
- **`teensy30.json`** -- Flags for Teensy 3.0 (MK20DX128, F_BUS=48MHz)
- **`teensylc.json`** -- Flags for Teensy LC (Cortex-M0+)
- **`reference/`** -- PlatformIO-extracted reference configs for validation

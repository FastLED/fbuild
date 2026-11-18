# Renesas RA MCU Configs

Data-driven configuration for Renesas RA boards, embedded at compile time via `include_str!()`.

## Files

- **`ra4m1.json`** -- Flags for RA4M1 (Cortex-M4, Arduino UNO R4 family, `arduino:renesas_uno`)
- **`ra6m5.json`** -- Flags for RA6M5 (Cortex-M33, FPv5-SP, Arduino Portenta C33, `arduino:renesas_portenta`);
  flags and link libraries follow that package's `boards.txt` (`portenta_c33`) — FastLED/fbuild#2054

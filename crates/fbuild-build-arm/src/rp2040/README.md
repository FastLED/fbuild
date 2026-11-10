# RP2040

RP2040/RP2350 platform build support (Raspberry Pi Pico).

Uses the generic ARM compiler/linker from `generic_arm/` with RP2040-specific
MCU configurations and the RP2040 Arduino cores framework.

`board_props.rs` applies Arduino-Pico board properties and resolves the variant
from the selected framework while preserving explicit project overrides.

`core_select.rs` rejects envs that select the Arduino Mbed OS RP2040 core
(stock `pico`/`nanorp2040connect` without `board_build.core = earlephilhower`)
with an actionable error; fbuild only implements Arduino-Pico (FastLED/fbuild#1977).

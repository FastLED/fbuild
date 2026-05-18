# STM32 orchestrator

`Stm32Orchestrator` plus the support modules that drive the STM32 build.

The orchestrator was split into a module directory to keep every `.rs` file
under the 1000-LOC CI gate. The public API is unchanged — re-exports from
`stm32/mod.rs` and the `stm32::orchestrator::{Stm32Orchestrator, create,
is_stm32_project}` paths still resolve as before.

## Submodules

- `arduino_mbed.rs` — Arduino mbed-core build path (GIGA, PORTENTA_H7_M7,
  NICLA_VISION, OPTA, GENERIC_STM32H747_M4).
- `framework_props.rs` — STM32duino `boards.txt` parser with `menu.*` scope
  resolution and `{build.*}` template substitution.
- `includes.rs` — CMSIS/HAL include-path assembly and small dedupe / define
  helpers shared by both build paths.
- `variant_files.rs` — pick the right `variant_*.{h,cpp}` and
  `PeripheralPins_*.c` so the linker sees one variant per build.
- `mod.rs` — primary STM32duino flow (phases 1-10) and the `Stm32Orchestrator`
  trait impl. Holds the unit tests.

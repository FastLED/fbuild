# `espidf_component_riscv` fixture

A genuine ESP-IDF component project for the RISC-V targets, used by the
delegated-build end-to-end test
(`crates/fbuild-build-esp/src/esp32/espidf_delegate_e2e_tests.rs`). The sources
are the probe application from FastLED/fbuild#1878, re-homed as a project with
its own root `CMakeLists.txt`, which is what the delegated build requires.

- `main` registers with `idf_component_register(... REQUIRES idf_probe log)` and
  fails to compile unless `sdkconfig.defaults` reached the IDF build;
- `components/idf_probe` is a second, project-owned component;
- `platformio.ini` has one environment per chip: `esp32c3`, `esp32c5`,
  `esp32c6`, `esp32p4` (pre-rev-3 silicon) and `esp32p4-r3`, which must be
  refused — the pinned ESP-IDF v5.5.1 cannot target rev-3 silicon.

`sdkconfig.defaults` deliberately names no `CONFIG_IDF_TARGET`: the target comes
from each environment's board.

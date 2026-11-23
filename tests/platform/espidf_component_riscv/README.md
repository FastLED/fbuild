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
  `esp32c6`, `esp32p4` (pre-rev-3 silicon) and `esp32p4-r3` (rev-3 silicon).
  Both P4 environments build from the same release; the generated `sdkconfig`
  must show the family each board declares (`CONFIG_ESP32P4_REV_MIN_FULL=0` vs
  `=300`, FastLED/fbuild#2133).

`sdkconfig.defaults` deliberately names no `CONFIG_IDF_TARGET`: the target comes
from each environment's board.

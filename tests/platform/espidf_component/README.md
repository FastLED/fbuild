# `espidf_component` fixture

A genuine ESP-IDF component project, used by the delegated-build end-to-end
test (`crates/fbuild-build-esp/src/esp32/espidf_delegate_e2e_tests.rs`,
FastLED/fbuild#1971):

- root `CMakeLists.txt` includes IDF's `project.cmake` — fbuild does not parse
  it, `idf.py` does;
- `main` registers with `idf_component_register(... REQUIRES mycomp)`;
- a second component, `components/mycomp`, with its own sources and include
  dir;
- `app_main()` only: this project has no Arduino `setup()`/`loop()`, which is
  exactly why the Arduino path cannot build it (#1730).

Without `board_build.build_system = espidf` this project must fail fast; the
opt-in is the only thing that makes fbuild run IDF's own build.

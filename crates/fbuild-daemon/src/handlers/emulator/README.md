# Emulator Handlers

`POST /api/deploy` emulator targets, `POST /api/test-emu`, and the `EmulatorRunner` abstraction. Split into focused submodules to stay under the 900-LOC per-file gate.

- **`mod.rs`** -- Submodule declarations and public re-exports preserving `handlers::emulator::*` paths.
- **`shared.rs`** -- Streaming subprocess runner (`run_qemu_process`), `ProcessEvent`/`RunQemuOptions`/`QemuRunResult`, `EmulatorRunConfig`, `monitor_outcome_to_emulator`, `qemu_session_dir`, ESP32 toolchain GCC resolution.
- **`avr8js_web.rs`** -- Browser-side HTML/JS/session.json/firmware.hex handlers and the `Avr8jsSessionManifest` type.
- **`avr8js_npm.rs`** -- `find_node`, `ensure_avr8js_npm[_in]`, `Avr8jsCachePrep`, and the `FBUILD_REFRESH_EMU_CACHE` env-var helpers.
- **`avr8js_headless.rs`** -- `run_avr8js_headless` (Node.js subprocess streaming) plus `AVR8JS_HEADLESS_MJS`.
- **`avr8js_deploy.rs`** -- `DeployAvr8jsRequest` and the `deploy_avr8js` handler (stages firmware, optionally runs headless).
- **`qemu_deploy.rs`** -- `DeployQemuRequest`, the `deploy_qemu` handler, `resolve_esp_qemu_for_mcu`, `check_qemu_flash_mode`, `is_qemu_supported_esp32_mcu`.
- **`runners.rs`** -- `EmulatorRunner` trait and concrete `QemuRunner` / `Avr8jsRunner` / `SimavrRunner` impls (plus `find_simavr`).
- **`select.rs`** -- `select_runner` and the `test_emu` build-then-emulate handler.
- **`tests_outcome.rs`** -- `monitor_outcome_to_emulator` mapping tests.
- **`tests_process.rs`** -- Subprocess runner tests, an ignored ESP32-S3 fixture integration test, and simavr/avr8js streaming tests.
- **`tests_select_runner.rs`** -- `select_runner` and `is_qemu_supported_esp32_mcu` coverage.
- **`tests_npm_cache.rs`** -- avr8js npm cache integrity and `ensure_avr8js_npm_in` error-message tests.

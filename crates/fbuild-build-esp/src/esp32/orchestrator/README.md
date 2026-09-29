# esp32::orchestrator

ESP32 build orchestrator split into focused submodules so no single file
exceeds the 1000-LOC gate.

| File | Responsibility |
|---|---|
| `mod.rs` | Module root; exposes `Esp32Orchestrator` and the small public helpers (`create`, `is_esp32_project`, `cdc_on_boot_enabled`, `warn_if_cdc_on_boot`). |
| `build.rs` | `impl BuildOrchestrator for Esp32Orchestrator`. Top-level phase wiring. |
| `packages.rs` | pioarduino platform / framework / toolchain resolution. |
| `job_pool.rs` | The build's single compile job pool (FastLED/fbuild#1559): one `tokio::join!` of lib_deps, framework libs, project-as-library, core + sketch, local libs and boot artifacts on one job gate; `link_order` keeps the archive order. Tests in `job_pool_tests.rs`. |
| `compile_phases.rs` | Core + sketch compile on the caller's shared job gate. |
| `framework_libs.rs` | Built-in Arduino libraries shipped with the framework: sync prepare (cache hits, failure skips), concurrent compile on the shared gate, in-order finish. Tests in `framework_libs_tests.rs`. |
| `local_libs.rs` | Libraries from the project's `lib/` directory, and the project-as-library. |
| `embed.rs` | `objcopy --input-target binary` conversion of embedded files. |
| `embed_stage.rs` | `.lnk` resolution and target selection wrapper around `embed`. |
| `boot_artifacts.rs` | Produces `bootloader.bin`, `partitions.bin`, `boot_app0.bin`. |
| `fingerprint.rs` | Serialised metadata struct used for the fast-path hash. |
| `helpers.rs` | Failure markers, signature, profile labels, compile-db freshness. (Flag merging — `apply_user_flags` / `apply_overlay_flags` — moved up to `crate::flag_overlay` so the nxplpc orchestrator can share it; see fbuild#587.) |
| `cdc.rs` | USB-CDC-on-boot warning + small public convenience helpers. |
| `tests.rs` | Unit tests for the helpers + the public surface. |

External crates continue to reference items at the original path
`fbuild_build::esp32::orchestrator::Esp32Orchestrator`; that public API is
preserved by re-exports in `mod.rs`.

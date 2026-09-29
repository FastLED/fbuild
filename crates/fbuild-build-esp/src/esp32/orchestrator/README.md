# esp32::orchestrator

ESP32 build orchestrator split into focused submodules so no single file
exceeds the 1000-LOC gate.

| File | Responsibility |
|---|---|
| `mod.rs` | Module root; exposes `Esp32Orchestrator` and the small public helpers (`create`, `is_esp32_project`, `cdc_on_boot_enabled`, `warn_if_cdc_on_boot`). |
| `build.rs` | `impl BuildOrchestrator for Esp32Orchestrator`. Top-level phase wiring. |
| `packages.rs` | pioarduino platform / framework / toolchain resolution. |
| `framework_libs.rs` | Compiles built-in Arduino libraries shipped with the framework. |
| `local_libs.rs` | Compiles libraries from the project's `lib/` directory. |
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

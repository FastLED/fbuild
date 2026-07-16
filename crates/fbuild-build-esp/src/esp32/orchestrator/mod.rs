//! ESP32 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (esp32dev/esp32c6/etc.)
//! 3. Load MCU config from embedded JSON
//! 4. Ensure ESP32 platform (pioarduino)
//! 5. Resolve + ensure ESP32 toolchain via metadata
//! 6. Ensure ESP32 framework (Arduino core + ESP-IDF SDK libs)
//! 7. Setup build directories
//! 8. Collect include paths: core + variant + SDK (305+) + user src
//! 9. Download + compile library dependencies
//! 10. Scan sources (sketch + core)
//! 11. Compile core sources
//! 12. Compile sketch sources
//! 13. Link (with linker scripts + SDK libs + library archives)
//! 14. Convert to .bin
//! 15. Copy bootloader.bin + partitions.bin
//! 16. Size reporting
//!
//! The orchestrator is split across sibling files in this directory to keep
//! each one under the 1000-LOC gate. `build.rs` contains the top-level
//! `impl BuildOrchestrator`; everything else is helper modules.

mod boot_artifacts;
mod build;
mod cdc;
mod embed;
mod embed_stage;
mod fingerprint;
mod framework_library_cache;
mod framework_libs;
mod helpers;
mod local_libs;
mod packages;

#[cfg(test)]
mod tests;

/// ESP32 platform build orchestrator.
pub struct Esp32Orchestrator;

pub use cdc::{cdc_on_boot_enabled, create, is_esp32_project, warn_if_cdc_on_boot};

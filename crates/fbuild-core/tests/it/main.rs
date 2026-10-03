//! One integration-test binary for this crate (zackees/ci.yml RUST-005):
//! each top-level `tests/*.rs` file links the crate's whole dependency
//! graph separately, so these modules share one link instead.

mod cpu_profiling;
mod dep_identity;
mod platformio_avr_resolution;
mod platformio_ch32v_resolution;
mod platformio_esp8266_resolution;
mod platformio_package_resolution;
mod platformio_registry_system;

//! Emulator deploy handlers, runner abstractions, and the `POST /api/test-emu`
//! build-then-emulate flow.
//!
//! This module is split into focused submodules to keep individual files small.
//! The public API (`deploy_avr8js`, `deploy_qemu`, `test_emu`, the avr8js web
//! handlers, the `EmulatorRunner` trait, `select_runner`, and the request
//! structs) is re-exported here so external callers continue to access it via
//! `handlers::emulator::*`.

mod avr8js_deploy;
mod avr8js_headless;
mod avr8js_npm;
mod avr8js_web;
mod qemu_deploy;
mod runners;
mod select;
mod shared;

#[cfg(test)]
mod tests_npm_cache;
#[cfg(test)]
mod tests_outcome;
#[cfg(test)]
mod tests_process;
#[cfg(test)]
mod tests_select_runner;

// --- Public API re-exports (preserve `handlers::emulator::*` paths) ---

pub use avr8js_deploy::{DeployAvr8jsRequest, deploy_avr8js};
pub use avr8js_web::{avr8js_app_js, avr8js_firmware_hex, avr8js_page, avr8js_session_json};
pub use qemu_deploy::{DeployQemuRequest, deploy_qemu};
pub use runners::{Avr8jsRunner, EmulatorRunner, QemuRunner, SimavrRunner};
pub use select::{select_runner, test_emu};
pub use shared::EmulatorRunConfig;

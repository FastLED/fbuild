//! Native ESP32 `verify-flash` **and** `write-flash` implementations
//! backed by the [`espflash`] crate. Alternatives to the default
//! [`crate::esp32::Esp32Deployer`] path, which shells out to Python
//! `esptool`.
//!
//! # Why (issue #66)
//!
//! `esptool.py` spends ~1 s on Python interpreter startup plus another
//! ~0.5 s on subprocess/stub-flasher handshake before it even issues the
//! first real command. Calling `espflash` in-process skips both — we
//! saved ~4 s on a cold 2.4 MB ESP32-S3 verify in the first half of this
//! work, and a full write gets the same baseline savings plus a
//! progress stream that the daemon can surface without scraping
//! subprocess stdout.
//!
//! # Scope
//!
//! * `verify-flash` — three regions (bootloader / partitions /
//!   firmware), same [`crate::esp32::VerifyOutcome`] semantics as the
//!   esptool path.
//! * `write-flash` — same three regions, same
//!   [`crate::DeploymentResult`]/[`crate::DeployOutcome`] shape as the
//!   esptool path. Progress callbacks from espflash are bridged into
//!   `tracing` so the daemon's existing log plumbing picks them up
//!   without any new API surface. Full WebSocket progress frames are a
//!   follow-up — see the `progress` submodule.
//!
//! # Serial-port lease
//!
//! The daemon pre-empts monitor sessions via
//! [`fbuild_serial::SharedSerialManager::preempt_for_deploy`] before
//! calling into this module. `preempt_for_deploy` explicitly closes the
//! OS-level port handle, so we can open our own here — exactly the same
//! way the existing esptool-subprocess path does. No second lease is
//! held concurrently.
//!
//! # Opt-in
//!
//! `verify-flash` is guarded by
//! [`crate::esp32::Esp32Deployer::with_native_verify`] (daemon env:
//! `FBUILD_USE_ESPFLASH_VERIFY`), and `write-flash` by
//! [`crate::esp32::Esp32Deployer::with_native_write`] (daemon env:
//! `FBUILD_USE_ESPFLASH_WRITE`). The two flags are independent —
//! users can flip one without the other while the native write path
//! accumulates bench time on every ESP32 family member.
//!
//! # Module layout
//!
//! Split into submodules so no single file exceeds the workspace LOC
//! gate. Public surface (`try_verify_deployment_native`,
//! `try_write_deployment_native`, `NativeVerifyRegion`,
//! `NativeWriteRegion`, `collect_standard_regions`,
//! `collect_standard_write_regions`,
//! `collect_selected_write_regions`) is re-exported from this `mod.rs`
//! so external callers see the same paths as before.

mod progress;
mod transport;
mod types;
mod verify;
mod write;

pub use types::{NativeVerifyRegion, NativeWriteRegion};
pub use verify::{collect_standard_regions, try_verify_deployment_native};
pub use write::{
    collect_selected_write_regions, collect_standard_write_regions, try_write_deployment_native,
};

#[cfg(test)]
mod tests;

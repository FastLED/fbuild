//! ESP32 deployer using esptool.py.
//!
//! Flashes firmware to ESP32 boards via serial port using esptool.
//! Bootloader offset varies by MCU:
//! - `0x1000`: esp32, esp32s2
//! - `0x0`: esp32c2, esp32c3, esp32c5, esp32c6, esp32h2, esp32s3
//! - `0x2000`: esp32p4
//!
//! This file is the module entrypoint; the implementation is split
//! across sibling files to keep each one under the LOC gate:
//!
//! * `deployer` — [`Esp32Deployer`], [`EsptoolParams`], esptool argv,
//!   verify/write paths, and the [`crate::Deployer`] impl.
//! * `verify`   — `FlashRegion`, `RegionVerifyResult`, `VerifyOutcome`,
//!   and the `parse_verify_regions` helper.
//! * `region`   — single file-at-offset flash/verify for `fbuild uploadfs`
//!   ([`RegionFlashResult`]).
//! * `qemu`     — QEMU flash-image assembly and argv builders.
//! * `image`    — ESP image header constants, byte patching, checksum
//!   and SHA-256 trailer repair, and raw binary I/O helpers.
//! * `native_usb` — 1200-bps bootloader entry for S2/S3 TinyUSB CDC apps.
//! * `parse`    — Hex offset / flash-size string parsers shared across
//!   the submodules.
//!
//! All submodules are private; only the types re-exported below form
//! the public API.

mod deployer;
mod image;
mod native_usb;
mod parse;
mod qemu;
mod region;
#[cfg(test)]
mod region_tests;
#[cfg(test)]
mod tests;
mod verify;

pub use deployer::{Esp32Deployer, EsptoolParams};
pub use qemu::{
    build_qemu_args, build_qemu_esp32s3_args, create_qemu_flash_image,
    resolve_qemu_flash_size_bytes,
};
pub use region::RegionFlashResult;
pub use verify::{FlashRegion, RegionVerifyResult, VerifyOutcome, parse_verify_regions};

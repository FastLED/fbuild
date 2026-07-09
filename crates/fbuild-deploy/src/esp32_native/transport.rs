//! Transport-layer helpers shared by the native verify and write paths:
//! chip / reset-string parsing, local MD5, USB port discovery, and the
//! per-region stdout renderer.

use std::str::FromStr;

use espflash::connection::{ResetAfterOperation, ResetBeforeOperation};
use espflash::target::Chip;
use md5::{Digest, Md5};
use serialport::{SerialPortType, UsbPortInfo};

use fbuild_core::{FbuildError, Result};

use crate::esp32::{FlashRegion, RegionVerifyResult};

/// Parse a chip name string (`"esp32s3"`, `"esp32c6"`, ...) into
/// espflash's [`Chip`] enum.
///
/// espflash derives [`strum::EnumString`] with `serialize_all =
/// "lowercase"` on `Chip`, so this is a thin wrapper around the existing
/// `FromStr` impl. Kept as a named helper so error messages point at
/// this module instead of at espflash internals.
pub(super) fn parse_chip(name: &str) -> Result<Chip> {
    Chip::from_str(&name.to_ascii_lowercase()).map_err(|_| {
        FbuildError::DeployFailed(format!(
            "native verify: unknown chip name '{}' (espflash does not recognize it)",
            name
        ))
    })
}

pub(super) fn parse_before_reset(s: &str) -> Result<ResetBeforeOperation> {
    // Match the esptool CLI spellings we already accept in board JSON.
    match s {
        "default-reset" | "default_reset" => Ok(ResetBeforeOperation::DefaultReset),
        "no-reset" | "no_reset" => Ok(ResetBeforeOperation::NoReset),
        "no-reset-no-sync" | "no_reset_no_sync" => Ok(ResetBeforeOperation::NoResetNoSync),
        "usb-reset" | "usb_reset" => Ok(ResetBeforeOperation::UsbReset),
        other => Err(FbuildError::DeployFailed(format!(
            "native verify: unsupported before-reset mode '{}'",
            other
        ))),
    }
}

pub(super) fn parse_after_reset(s: &str) -> Result<ResetAfterOperation> {
    match s {
        "hard-reset" | "hard_reset" => Ok(ResetAfterOperation::HardReset),
        "no-reset" | "no_reset" => Ok(ResetAfterOperation::NoReset),
        "no-reset-no-stub" | "no_reset_no_stub" => Ok(ResetAfterOperation::NoResetNoStub),
        "watchdog-reset" | "watchdog_reset" => Ok(ResetAfterOperation::WatchdogReset),
        other => Err(FbuildError::DeployFailed(format!(
            "native verify: unsupported after-reset mode '{}'",
            other
        ))),
    }
}

/// Compute MD5 of a local buffer and pack it into a `u128` in the same
/// byte order espflash's `checksum_md5` returns. espflash parses the
/// 16-byte on-chip digest as little-endian `u128`, so we do the same
/// here; equality over the packed ints is equivalent to equality over
/// the 16 digest bytes.
pub(super) fn local_md5(bytes: &[u8]) -> u128 {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let arr: [u8; 16] = digest.into();
    u128::from_le_bytes(arr)
}

/// Best-effort USB VID/PID lookup for the opened port, mirroring
/// espflash's own CLI fallback. Failure → zeros, which just means
/// reset-strategy selection uses generic defaults.
pub(super) fn discover_usb_port_info(port: &str) -> UsbPortInfo {
    match serialport::available_ports() {
        Ok(list) => {
            for p in list {
                if p.port_name == port {
                    if let SerialPortType::UsbPort(info) = p.port_type {
                        return info;
                    }
                }
            }
        }
        Err(e) => {
            tracing::debug!("native verify: available_ports failed: {}", e);
        }
    }
    UsbPortInfo {
        vid: 0,
        pid: 0,
        serial_number: None,
        manufacturer: None,
        product: None,
        interface: None,
    }
}

/// Render a compact text description of the per-region results that
/// callers can log or return in `VerifyOutcome::Match::stdout`. Keeps
/// the outcome surface identical between the esptool and native paths
/// for anything that reads `stdout` for display.
pub(super) fn render_native_stdout(results: &[RegionVerifyResult]) -> String {
    let mut out = String::new();
    for r in results {
        let name = match r.region {
            FlashRegion::Bootloader => "bootloader",
            FlashRegion::Partitions => "partitions",
            FlashRegion::Firmware => "firmware",
        };
        let verdict = if r.matched {
            "Verification successful (digest matched)."
        } else {
            "Verification failed (digest mismatch)."
        };
        out.push_str(&format!("native verify: {}: {}\n", name, verdict));
    }
    out
}

pub(super) fn region_name(r: FlashRegion) -> &'static str {
    match r {
        FlashRegion::Bootloader => "bootloader",
        FlashRegion::Partitions => "partitions",
        FlashRegion::Firmware => "firmware",
    }
}

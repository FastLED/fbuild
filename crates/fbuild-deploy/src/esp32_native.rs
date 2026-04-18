//! Native ESP32 `verify-flash` implementation backed by the [`espflash`]
//! crate. Alternative to the default [`super::esp32::Esp32Deployer`]
//! path, which shells out to Python `esptool`.
//!
//! # Why (issue #66)
//!
//! `esptool.py verify-flash` spends ~1 s on Python interpreter startup
//! plus another ~0.5 s on subprocess/stub-flasher handshake before it
//! even issues the `FLASH_MD5SUM` command. Calling `espflash` in-process
//! skips both, dropping a cold verify on a 2.4 MB ESP32-S3 image from
//! ~5.9 s to a projected ~1.5–2 s.
//!
//! # Scope for this PR
//!
//! * **In scope**: `verify-flash` only — same three regions
//!   (bootloader / partitions / firmware) as the esptool path, same
//!   [`super::esp32::VerifyOutcome`] result, same `Match` vs
//!   `Mismatch { regions }` semantics.
//! * **Out of scope (follow-up)**: `write-flash`. The stub flasher's
//!   write path needs a progress-callback bridge to the daemon's
//!   WebSocket log stream and more careful error recovery than a
//!   read-only MD5 comparison; splitting the two keeps this PR
//!   reviewable.
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
//! This path is guarded by [`Esp32Deployer::use_native_verify`], which
//! the daemon wires to the `FBUILD_USE_ESPFLASH_VERIFY` environment
//! variable. Default stays on the esptool subprocess path so users on
//! unusual setups keep their escape hatch.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::target::Chip;
use md5::{Digest, Md5};
use serialport::{FlowControl, SerialPortType, UsbPortInfo};

use fbuild_core::{FbuildError, Result};

use crate::esp32::{FlashRegion, RegionVerifyResult, VerifyOutcome};

/// A single region (flash offset + local firmware file) to verify.
#[derive(Debug, Clone)]
pub struct NativeVerifyRegion {
    pub region: FlashRegion,
    pub offset: u32,
    pub path: std::path::PathBuf,
}

/// Native `verify-flash` — reads each file from disk, asks the stub
/// flasher to compute `FLASH_MD5SUM` over the same region on-chip, and
/// reports per-region match/mismatch.
///
/// On `Match` the chip is hard-reset (matching esptool's
/// `--after hard-reset`) so callers can treat a `Match` as "device is
/// now running the requested firmware" just like the subprocess path.
///
/// Port ownership: caller must ensure the OS-level serial port is free
/// before calling (the daemon does this via `preempt_for_deploy`). The
/// opened handle is closed on function return.
///
/// `before_reset` / `after_reset` accept the same strings the esptool
/// path uses (`default-reset`, `no-reset`, `no-reset-no-sync`,
/// `usb-reset` for before; `hard-reset`, `no-reset`, `no-reset-no-stub`,
/// `watchdog-reset` for after) so the daemon wiring doesn't need a
/// separate config surface.
#[allow(clippy::too_many_arguments)]
pub fn try_verify_deployment_native(
    chip_name: &str,
    port: &str,
    baud: u32,
    before_reset: &str,
    after_reset: &str,
    regions: &[NativeVerifyRegion],
    bootloader_offset: u32,
    partitions_offset: u32,
    firmware_offset: u32,
) -> Result<VerifyOutcome> {
    // Map config strings → espflash enums. We intentionally keep the
    // surface narrow: anything outside the supported set is a hard
    // error so a typo in a board JSON doesn't silently degrade to
    // default-reset.
    let chip = parse_chip(chip_name)?;
    let before = parse_before_reset(before_reset)?;
    let after = parse_after_reset(after_reset)?;

    // Open the port at 115_200 (espflash renegotiates to `baud` after
    // stub upload). `open_native` returns the platform-specific `Port`
    // (COMPort / TTYPort) expected by `Connection::new`.
    let serial_port = serialport::new(port, 115_200)
        .flow_control(FlowControl::None)
        .timeout(Duration::from_secs(3))
        .open_native()
        .map_err(|e| {
            FbuildError::DeployFailed(format!(
                "native verify: failed to open serial port {}: {}",
                port, e
            ))
        })?;

    // Look up the USB VID/PID for the port so reset strategy selection
    // in espflash (USB-JTAG vs classic) picks the right sequence.
    // Missing entries default to zero, matching espflash's own CLI.
    let port_info = discover_usb_port_info(port);

    let connection = Connection::new(serial_port, port_info, after, before, baud);

    // `Flasher::connect` handles reset, sync, chip detect, stub upload,
    // and baud renegotiation. Errors here are fatal — on real hardware
    // we treat them as "verify couldn't run, fall back to full flash"
    // at the caller, so we surface them via `FbuildError` rather than
    // embedding them in a `Mismatch`.
    let use_stub = true;
    let mut flasher = Flasher::connect(
        connection,
        use_stub,
        /* verify */ false, // we do our own per-region verify below
        /* skip   */ false,
        Some(chip),
        Some(baud),
    )
    .map_err(|e| {
        FbuildError::DeployFailed(format!(
            "native verify: espflash connect failed on {}: {}",
            port, e
        ))
    })?;

    // Compute per-region verdicts.
    let mut results: Vec<RegionVerifyResult> = Vec::with_capacity(regions.len());
    for r in regions {
        let bytes = std::fs::read(&r.path).map_err(|e| {
            FbuildError::DeployFailed(format!(
                "native verify: failed to read {}: {}",
                r.path.display(),
                e
            ))
        })?;
        let local = local_md5(&bytes);
        let remote = flasher
            .checksum_md5(r.offset, bytes.len() as u32)
            .map_err(|e| {
                FbuildError::DeployFailed(format!(
                    "native verify: FLASH_MD5SUM failed at 0x{:x}: {}",
                    r.offset, e
                ))
            })?;
        let matched = remote == local;
        tracing::debug!(
            port,
            region = ?r.region,
            offset = format!("0x{:x}", r.offset),
            size = bytes.len(),
            matched,
            "native verify region result"
        );
        results.push(RegionVerifyResult {
            region: r.region,
            matched,
        });
    }

    // Keep the three offsets threaded through even though we no longer
    // parse them from stdout — callers of `VerifyOutcome::Mismatch` use
    // `regions` directly and ignore stdout/stderr when they came from
    // the native path.
    let _ = (bootloader_offset, partitions_offset, firmware_offset);

    let all_match = !results.is_empty() && results.iter().all(|r| r.matched);

    // Hard-reset the chip back into the app on success so behavior
    // matches the esptool `--after hard-reset` contract. On mismatch we
    // leave the reset policy to the subsequent flash call.
    if all_match {
        let mut connection = flasher.into_connection();
        if let Err(e) = connection.reset_after(use_stub, chip) {
            tracing::warn!(port, "native verify: reset_after failed: {}", e);
        }
    }

    if all_match {
        Ok(VerifyOutcome::Match {
            stdout: render_native_stdout(&results),
            stderr: String::new(),
        })
    } else {
        Ok(VerifyOutcome::Mismatch {
            stdout: render_native_stdout(&results),
            stderr: String::new(),
            regions: results,
        })
    }
}

/// Parse a chip name string (`"esp32s3"`, `"esp32c6"`, ...) into
/// espflash's [`Chip`] enum.
///
/// espflash derives [`strum::EnumString`] with `serialize_all =
/// "lowercase"` on `Chip`, so this is a thin wrapper around the existing
/// `FromStr` impl. Kept as a named helper so error messages point at
/// this module instead of at espflash internals.
fn parse_chip(name: &str) -> Result<Chip> {
    Chip::from_str(&name.to_ascii_lowercase()).map_err(|_| {
        FbuildError::DeployFailed(format!(
            "native verify: unknown chip name '{}' (espflash does not recognize it)",
            name
        ))
    })
}

fn parse_before_reset(s: &str) -> Result<ResetBeforeOperation> {
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

fn parse_after_reset(s: &str) -> Result<ResetAfterOperation> {
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
fn local_md5(bytes: &[u8]) -> u128 {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let arr: [u8; 16] = digest.into();
    u128::from_le_bytes(arr)
}

/// Best-effort USB VID/PID lookup for the opened port, mirroring
/// espflash's own CLI fallback. Failure → zeros, which just means
/// reset-strategy selection uses generic defaults.
fn discover_usb_port_info(port: &str) -> UsbPortInfo {
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
    }
}

/// Render a compact text description of the per-region results that
/// callers can log or return in `VerifyOutcome::Match::stdout`. Keeps
/// the outcome surface identical between the esptool and native paths
/// for anything that reads `stdout` for display.
fn render_native_stdout(results: &[RegionVerifyResult]) -> String {
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

/// Collect the standard three-region set from a firmware path. The
/// caller supplies the offsets (parsed once from board config by
/// [`super::esp32::Esp32Deployer`]).
///
/// Mirrors [`super::esp32::Esp32Deployer::build_verify_flash_args`]:
/// bootloader and partitions are optional (absent files are skipped);
/// firmware is mandatory.
pub fn collect_standard_regions(
    firmware_path: &Path,
    bootloader_offset: u32,
    partitions_offset: u32,
    firmware_offset: u32,
) -> Vec<NativeVerifyRegion> {
    let build_dir = firmware_path.parent().unwrap_or_else(|| Path::new("."));
    let bootloader_path = build_dir.join("bootloader.bin");
    let partitions_path = build_dir.join("partitions.bin");

    let mut out = Vec::with_capacity(3);
    if bootloader_path.exists() {
        out.push(NativeVerifyRegion {
            region: FlashRegion::Bootloader,
            offset: bootloader_offset,
            path: bootloader_path,
        });
    }
    if partitions_path.exists() {
        out.push(NativeVerifyRegion {
            region: FlashRegion::Partitions,
            offset: partitions_offset,
            path: partitions_path,
        });
    }
    out.push(NativeVerifyRegion {
        region: FlashRegion::Firmware,
        offset: firmware_offset,
        path: firmware_path.to_path_buf(),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chip_accepts_lowercase_family_members() {
        // All ten currently-supported chips must round-trip through
        // our wrapper. Guards against a future espflash bump that
        // renames a variant.
        for name in [
            "esp32", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61", "esp32h2", "esp32p4",
            "esp32s2", "esp32s3",
        ] {
            parse_chip(name)
                .unwrap_or_else(|e| panic!("chip '{}' must parse in espflash: {:?}", name, e));
        }
    }

    #[test]
    fn parse_chip_accepts_uppercase_because_we_lowercase_first() {
        parse_chip("ESP32S3").unwrap();
        parse_chip("Esp32C6").unwrap();
    }

    #[test]
    fn parse_chip_rejects_unknown() {
        assert!(parse_chip("esp99").is_err());
    }

    #[test]
    fn parse_before_reset_covers_esptool_spellings() {
        assert!(matches!(
            parse_before_reset("default-reset").unwrap(),
            ResetBeforeOperation::DefaultReset
        ));
        assert!(matches!(
            parse_before_reset("no-reset").unwrap(),
            ResetBeforeOperation::NoReset
        ));
        assert!(matches!(
            parse_before_reset("usb-reset").unwrap(),
            ResetBeforeOperation::UsbReset
        ));
        assert!(parse_before_reset("bogus").is_err());
    }

    #[test]
    fn parse_after_reset_covers_esptool_spellings() {
        assert!(matches!(
            parse_after_reset("hard-reset").unwrap(),
            ResetAfterOperation::HardReset
        ));
        assert!(matches!(
            parse_after_reset("no-reset").unwrap(),
            ResetAfterOperation::NoReset
        ));
        assert!(parse_after_reset("bogus").is_err());
    }

    #[test]
    fn local_md5_matches_known_vector() {
        // RFC 1321 test vector: MD5("") = d41d8cd98f00b204e9800998ecf8427e
        let empty = local_md5(b"");
        let expected_bytes: [u8; 16] = [
            0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
            0x42, 0x7e,
        ];
        assert_eq!(empty, u128::from_le_bytes(expected_bytes));

        // MD5("abc") = 900150983cd24fb0d6963f7d28e17f72
        let abc = local_md5(b"abc");
        let expected_bytes: [u8; 16] = [
            0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1,
            0x7f, 0x72,
        ];
        assert_eq!(abc, u128::from_le_bytes(expected_bytes));
    }

    #[test]
    fn collect_standard_regions_skips_missing_optional_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firmware").unwrap();

        let regions = collect_standard_regions(&fw, 0x0, 0x8000, 0x10000);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].region, FlashRegion::Firmware);
        assert_eq!(regions[0].offset, 0x10000);
    }

    #[test]
    fn collect_standard_regions_includes_optional_files_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firmware").unwrap();
        std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
        std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();

        let regions = collect_standard_regions(&fw, 0x0, 0x8000, 0x10000);
        assert_eq!(regions.len(), 3);
        assert_eq!(regions[0].region, FlashRegion::Bootloader);
        assert_eq!(regions[0].offset, 0x0);
        assert_eq!(regions[1].region, FlashRegion::Partitions);
        assert_eq!(regions[1].offset, 0x8000);
        assert_eq!(regions[2].region, FlashRegion::Firmware);
        assert_eq!(regions[2].offset, 0x10000);
    }

    #[test]
    fn render_native_stdout_mentions_all_regions() {
        let results = vec![
            RegionVerifyResult {
                region: FlashRegion::Bootloader,
                matched: true,
            },
            RegionVerifyResult {
                region: FlashRegion::Firmware,
                matched: false,
            },
        ];
        let out = render_native_stdout(&results);
        assert!(out.contains("bootloader"));
        assert!(out.contains("firmware"));
        assert!(out.contains("digest matched"));
        assert!(out.contains("digest mismatch"));
    }
}

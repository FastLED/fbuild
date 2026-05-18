//! Native `verify-flash` implementation backed by espflash.

use std::path::Path;
use std::time::Duration;

use espflash::connection::Connection;
use espflash::flasher::Flasher;
use serialport::FlowControl;

use fbuild_core::{FbuildError, Result};

use crate::esp32::{FlashRegion, RegionVerifyResult, VerifyOutcome};

use super::transport::{
    discover_usb_port_info, local_md5, parse_after_reset, parse_before_reset, parse_chip,
    render_native_stdout,
};
use super::types::NativeVerifyRegion;

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

/// Collect the standard three-region set from a firmware path. The
/// caller supplies the offsets (parsed once from board config by
/// [`super::super::esp32::Esp32Deployer`]).
///
/// Mirrors [`super::super::esp32::Esp32Deployer::build_verify_flash_args`]:
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

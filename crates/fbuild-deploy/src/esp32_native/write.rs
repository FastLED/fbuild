//! Native `write-flash` implementation backed by espflash, plus the
//! per-region collection helpers used by the daemon's selective flash
//! path.

use std::path::Path;
use std::time::Duration;

use espflash::connection::Connection;
use espflash::flasher::Flasher;
use serialport::FlowControl;

use fbuild_core::{FbuildError, Result};

use crate::esp32::FlashRegion;
use crate::{DeployOutcome, DeploymentResult};

use super::progress::LoggingProgressBridge;
use super::transport::{
    discover_usb_port_info, parse_after_reset, parse_before_reset, parse_chip, region_name,
};
use super::types::NativeWriteRegion;

/// Native `write-flash` — reads each file from disk and streams it to
/// the chip via espflash's stub flasher. Same three regions
/// (bootloader / partitions / firmware) as the esptool path, same
/// [`DeploymentResult`] / [`DeployOutcome`] semantics so callers can
/// route between the two behind a single flag.
///
/// Progress from espflash is bridged into `tracing` so the daemon's
/// existing log broadcaster surfaces it without new API surface (see
/// the private `LoggingProgressBridge` in `super::progress`).
/// Structured WebSocket progress frames are a follow-up: the bridge is
/// a drop-in replacement point for a richer callback without touching
/// any of the call sites.
///
/// On success the chip is hard-reset (matching esptool's
/// `--after hard-reset`) so callers can treat the `Ok` return as
/// "device is now running the requested firmware" without an extra
/// reset.
///
/// Port ownership: caller must ensure the OS-level serial port is free
/// before calling (the daemon does this via `preempt_for_deploy`). The
/// opened handle is closed on function return.
///
/// Error recovery: on any region failure we short-circuit, log which
/// region failed with its flash offset, return a failed
/// [`DeploymentResult`], and let the caller decide whether to retry via
/// esptool. Partial writes are never silently swallowed.
#[allow(clippy::too_many_arguments)]
pub fn try_write_deployment_native(
    chip_name: &str,
    port: &str,
    baud: u32,
    before_reset: &str,
    after_reset: &str,
    regions: &[NativeWriteRegion],
    selective: bool,
) -> Result<DeploymentResult> {
    let chip = parse_chip(chip_name)?;
    let before = parse_before_reset(before_reset)?;
    let after = parse_after_reset(after_reset)?;

    if regions.is_empty() {
        return Err(FbuildError::DeployFailed(
            "native write: called with no regions; at least firmware is required".to_string(),
        ));
    }

    let serial_port = serialport::new(port, 115_200)
        .flow_control(FlowControl::None)
        // Writes are much longer than verifies; the stub flasher can
        // take tens of seconds between UART responses while erasing
        // the partition table sector on a cold boot. 10s matches
        // espflash's own CLI default.
        .timeout(Duration::from_secs(10))
        .open_native()
        .map_err(|e| {
            FbuildError::DeployFailed(format!(
                "native write: failed to open serial port {}: {}",
                port, e
            ))
        })?;

    let port_info = discover_usb_port_info(port);
    let connection = Connection::new(serial_port, port_info, after, before, baud);

    let use_stub = true;
    let mut flasher = Flasher::connect(
        connection,
        use_stub,
        /* verify */
        false, // espflash's own post-write verify; we rely on the separate verify path
        /* skip   */ false,
        Some(chip),
        Some(baud),
    )
    .map_err(|e| {
        FbuildError::DeployFailed(format!(
            "native write: espflash connect failed on {}: {}",
            port, e
        ))
    })?;

    let mut bridge = LoggingProgressBridge::new(port);
    let mut rendered = String::new();
    let mut bytes_written: u64 = 0;

    for r in regions {
        let bytes = std::fs::read(&r.path).map_err(|e| {
            FbuildError::DeployFailed(format!(
                "native write: failed to read {}: {}",
                r.path.display(),
                e
            ))
        })?;
        let size = bytes.len();
        bridge.enter_region(r.region);
        tracing::info!(
            port,
            region = ?r.region,
            offset = format!("0x{:x}", r.offset),
            size,
            "native write: writing region"
        );
        if let Err(e) = flasher.write_bin_to_flash(r.offset, &bytes, &mut bridge) {
            let msg = format!(
                "native write: region {:?} at 0x{:x} failed after {}/{} bytes: {}",
                r.region, r.offset, bridge.last_current, size, e
            );
            tracing::error!(port, "{}", msg);
            rendered.push_str(&msg);
            rendered.push('\n');
            return Ok(DeploymentResult {
                success: false,
                message: format!("espflash native write failed on {} ({})", port, chip_name),
                port: Some(port.to_string()),
                stdout: rendered,
                stderr: e.to_string(),
                outcome: outcome_for(selective, regions),
            });
        }
        bytes_written += size as u64;
        rendered.push_str(&format!(
            "native write: {} at 0x{:x} ({} bytes) OK\n",
            region_name(r.region),
            r.offset,
            size
        ));
    }

    // Hard-reset (or whatever after_reset selects) the chip so the app
    // boots once the stub releases the bus. Mirrors esptool's
    // `--after hard-reset` contract.
    let mut connection = flasher.into_connection();
    if let Err(e) = connection.reset_after(use_stub, chip) {
        // A failed final reset is annoying but doesn't invalidate the
        // flash: the image is on-device, a manual power cycle will
        // boot it. Log and report success.
        tracing::warn!(port, "native write: reset_after failed: {}", e);
    }

    tracing::info!(
        port,
        bytes_written,
        regions = regions.len(),
        "native write: completed successfully"
    );

    Ok(DeploymentResult {
        success: true,
        message: format!(
            "firmware flashed to {} ({}) via espflash ({} bytes, {} region(s))",
            port,
            chip_name,
            bytes_written,
            regions.len()
        ),
        port: Some(port.to_string()),
        stdout: rendered,
        stderr: String::new(),
        outcome: outcome_for(selective, regions),
    })
}

/// Pick the correct [`DeployOutcome`] for a native write — preserves
/// the existing invariant that the full three-region path reports
/// `FullFlash` and selective rewrites carry the region list.
pub(super) fn outcome_for(selective: bool, regions: &[NativeWriteRegion]) -> DeployOutcome {
    if selective {
        DeployOutcome::SelectiveFlash {
            regions: regions.iter().map(|r| r.region).collect(),
        }
    } else {
        DeployOutcome::FullFlash
    }
}

/// Collect the full three-region write set (bootloader + partitions +
/// firmware where present) from a firmware path. Mirrors
/// [`super::verify::collect_standard_regions`] but returns
/// [`NativeWriteRegion`].
pub fn collect_standard_write_regions(
    firmware_path: &Path,
    bootloader_offset: u32,
    partitions_offset: u32,
    firmware_offset: u32,
) -> Vec<NativeWriteRegion> {
    let build_dir = firmware_path.parent().unwrap_or_else(|| Path::new("."));
    let bootloader_path = build_dir.join("bootloader.bin");
    let partitions_path = build_dir.join("partitions.bin");

    let mut out = Vec::with_capacity(3);
    if bootloader_path.exists() {
        out.push(NativeWriteRegion {
            region: FlashRegion::Bootloader,
            offset: bootloader_offset,
            path: bootloader_path,
        });
    }
    if partitions_path.exists() {
        out.push(NativeWriteRegion {
            region: FlashRegion::Partitions,
            offset: partitions_offset,
            path: partitions_path,
        });
    }
    out.push(NativeWriteRegion {
        region: FlashRegion::Firmware,
        offset: firmware_offset,
        path: firmware_path.to_path_buf(),
    });
    out
}

/// Collect a caller-chosen subset of write regions. Used by the daemon's
/// selective-flash path (post-verify-mismatch) so we don't rewrite
/// bootloader/partitions when only firmware differs. `requested` must
/// not be empty.
pub fn collect_selected_write_regions(
    firmware_path: &Path,
    bootloader_offset: u32,
    partitions_offset: u32,
    firmware_offset: u32,
    requested: &[FlashRegion],
) -> Result<Vec<NativeWriteRegion>> {
    if requested.is_empty() {
        return Err(FbuildError::DeployFailed(
            "native write: collect_selected_write_regions called with empty request".to_string(),
        ));
    }
    let build_dir = firmware_path.parent().unwrap_or_else(|| Path::new("."));
    let mut out = Vec::with_capacity(requested.len());
    for r in requested {
        let (path, offset) = match r {
            FlashRegion::Bootloader => (build_dir.join("bootloader.bin"), bootloader_offset),
            FlashRegion::Partitions => (build_dir.join("partitions.bin"), partitions_offset),
            FlashRegion::Firmware => (firmware_path.to_path_buf(), firmware_offset),
        };
        if !path.exists() {
            return Err(FbuildError::DeployFailed(format!(
                "native write: requested {:?} but {} is missing",
                r,
                path.display()
            )));
        }
        out.push(NativeWriteRegion {
            region: *r,
            offset,
            path,
        });
    }
    Ok(out)
}

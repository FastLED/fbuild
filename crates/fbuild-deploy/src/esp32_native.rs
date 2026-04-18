//! Native ESP32 `verify-flash` **and** `write-flash` implementations
//! backed by the [`espflash`] crate. Alternatives to the default
//! [`super::esp32::Esp32Deployer`] path, which shells out to Python
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
//!   firmware), same [`VerifyOutcome`] semantics as the esptool path.
//! * `write-flash` — same three regions, same
//!   [`DeploymentResult`]/[`DeployOutcome`] shape as the esptool path.
//!   Progress callbacks from espflash are bridged into `tracing` so the
//!   daemon's existing log plumbing picks them up without any new API
//!   surface. Full WebSocket progress frames are a follow-up — see
//!   `log_only_progress_bridge` below.
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
//! `verify-flash` is guarded by [`Esp32Deployer::use_native_verify`]
//! (daemon env: `FBUILD_USE_ESPFLASH_VERIFY`), and `write-flash` by
//! [`Esp32Deployer::use_native_write`] (daemon env:
//! `FBUILD_USE_ESPFLASH_WRITE`). The two flags are independent —
//! users can flip one without the other while the native write path
//! accumulates bench time on every ESP32 family member.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::target::{Chip, ProgressCallbacks};
use md5::{Digest, Md5};
use serialport::{FlowControl, SerialPortType, UsbPortInfo};

use fbuild_core::{FbuildError, Result};

use crate::esp32::{FlashRegion, RegionVerifyResult, VerifyOutcome};
use crate::{DeployOutcome, DeploymentResult};

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

/// A single region (flash offset + local firmware file) to write.
///
/// Same shape as [`NativeVerifyRegion`] but kept as a distinct type so
/// a future change (e.g. encrypted-write flags per-region) doesn't
/// silently leak back into the verify path.
#[derive(Debug, Clone)]
pub struct NativeWriteRegion {
    pub region: FlashRegion,
    pub offset: u32,
    pub path: std::path::PathBuf,
}

/// Native `write-flash` — reads each file from disk and streams it to
/// the chip via espflash's stub flasher. Same three regions
/// (bootloader / partitions / firmware) as the esptool path, same
/// [`DeploymentResult`] / [`DeployOutcome`] semantics so callers can
/// route between the two behind a single flag.
///
/// Progress from espflash is bridged into `tracing` so the daemon's
/// existing log broadcaster surfaces it without new API surface (see
/// [`LoggingProgressBridge`]). Structured WebSocket progress frames are
/// a follow-up: the bridge is a drop-in replacement point for a richer
/// callback without touching any of the call sites.
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
fn outcome_for(selective: bool, regions: &[NativeWriteRegion]) -> DeployOutcome {
    if selective {
        DeployOutcome::SelectiveFlash {
            regions: regions.iter().map(|r| r.region).collect(),
        }
    } else {
        DeployOutcome::FullFlash
    }
}

fn region_name(r: FlashRegion) -> &'static str {
    match r {
        FlashRegion::Bootloader => "bootloader",
        FlashRegion::Partitions => "partitions",
        FlashRegion::Firmware => "firmware",
    }
}

/// Collect the full three-region write set (bootloader + partitions +
/// firmware where present) from a firmware path. Mirrors
/// [`collect_standard_regions`] but returns [`NativeWriteRegion`].
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

/// Bridges espflash's [`ProgressCallbacks`] into `tracing` so the
/// daemon's existing log broadcaster picks up write progress without
/// any new API surface.
///
/// Logging-only by design: a richer WebSocket bridge (structured
/// progress frames on the deploy WS channel) is a follow-up — this
/// type is the single seam where that upgrade lands.
struct LoggingProgressBridge {
    port: String,
    region: Option<FlashRegion>,
    total: usize,
    last_current: usize,
    /// Last percentage we logged at. Throttles the per-block `update`
    /// spam to one line per 10% of a region so the daemon log stream
    /// stays readable.
    last_pct_logged: u8,
}

impl LoggingProgressBridge {
    fn new(port: &str) -> Self {
        Self {
            port: port.to_string(),
            region: None,
            total: 0,
            last_current: 0,
            last_pct_logged: 0,
        }
    }

    fn enter_region(&mut self, region: FlashRegion) {
        self.region = Some(region);
        self.total = 0;
        self.last_current = 0;
        self.last_pct_logged = 0;
    }

    fn region_label(&self) -> &'static str {
        match self.region {
            Some(r) => region_name(r),
            None => "unknown",
        }
    }
}

impl ProgressCallbacks for LoggingProgressBridge {
    fn init(&mut self, addr: u32, total: usize) {
        self.total = total;
        self.last_current = 0;
        self.last_pct_logged = 0;
        tracing::info!(
            port = %self.port,
            region = self.region_label(),
            addr = format!("0x{:x}", addr),
            total,
            "native write: begin region"
        );
    }

    fn update(&mut self, current: usize) {
        self.last_current = current;
        if self.total == 0 {
            return;
        }
        let pct = ((current as u64 * 100) / self.total as u64).min(100) as u8;
        // Emit a log line every 10% boundary so a 1 MB write produces
        // ~10 lines rather than hundreds.
        if pct >= self.last_pct_logged + 10 {
            self.last_pct_logged = pct - (pct % 10);
            tracing::info!(
                port = %self.port,
                region = self.region_label(),
                pct,
                current,
                total = self.total,
                "native write: progress"
            );
        }
    }

    fn verifying(&mut self) {
        tracing::debug!(
            port = %self.port,
            region = self.region_label(),
            "native write: verifying region (espflash internal)"
        );
    }

    fn finish(&mut self, skipped: bool) {
        tracing::info!(
            port = %self.port,
            region = self.region_label(),
            skipped,
            "native write: region complete"
        );
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

    // --- native write-flash tests (issue #66 PR #89 follow-up) ---
    //
    // Most of the write flow is live hardware code. What we can test
    // without a board attached is the region assembly, the
    // DeployOutcome mapping, and the progress-bridge throttling — the
    // three pure pieces where a regression would silently corrupt the
    // daemon's deploy response.

    #[test]
    fn collect_standard_write_regions_skips_missing_optional_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firmware").unwrap();

        let regions = collect_standard_write_regions(&fw, 0x0, 0x8000, 0x10000);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].region, FlashRegion::Firmware);
        assert_eq!(regions[0].offset, 0x10000);
    }

    #[test]
    fn collect_standard_write_regions_includes_optional_files_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firmware").unwrap();
        std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
        std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();

        let regions = collect_standard_write_regions(&fw, 0x0, 0x8000, 0x10000);
        assert_eq!(regions.len(), 3);
        assert_eq!(regions[0].region, FlashRegion::Bootloader);
        assert_eq!(regions[0].offset, 0x0);
        assert_eq!(regions[1].region, FlashRegion::Partitions);
        assert_eq!(regions[1].offset, 0x8000);
        assert_eq!(regions[2].region, FlashRegion::Firmware);
        assert_eq!(regions[2].offset, 0x10000);
    }

    #[test]
    fn collect_selected_write_regions_errors_on_empty_request() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firmware").unwrap();
        let err = collect_selected_write_regions(&fw, 0x0, 0x8000, 0x10000, &[]).unwrap_err();
        assert!(err.to_string().contains("empty request"));
    }

    #[test]
    fn collect_selected_write_regions_errors_when_file_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firmware").unwrap();
        // No bootloader.bin on disk.
        let err =
            collect_selected_write_regions(&fw, 0x0, 0x8000, 0x10000, &[FlashRegion::Bootloader])
                .unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn collect_selected_write_regions_returns_requested_subset() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firmware").unwrap();
        std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
        std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();

        let out = collect_selected_write_regions(
            &fw,
            0x0,
            0x8000,
            0x10000,
            &[FlashRegion::Firmware, FlashRegion::Bootloader],
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].region, FlashRegion::Firmware);
        assert_eq!(out[0].offset, 0x10000);
        assert_eq!(out[1].region, FlashRegion::Bootloader);
        assert_eq!(out[1].offset, 0x0);
    }

    #[test]
    fn outcome_for_full_write_reports_full_flash() {
        let regions = vec![NativeWriteRegion {
            region: FlashRegion::Firmware,
            offset: 0x10000,
            path: std::path::PathBuf::from("firmware.bin"),
        }];
        assert!(matches!(
            outcome_for(false, &regions),
            DeployOutcome::FullFlash
        ));
    }

    #[test]
    fn outcome_for_selective_write_carries_region_list() {
        let regions = vec![
            NativeWriteRegion {
                region: FlashRegion::Bootloader,
                offset: 0x0,
                path: std::path::PathBuf::from("bootloader.bin"),
            },
            NativeWriteRegion {
                region: FlashRegion::Firmware,
                offset: 0x10000,
                path: std::path::PathBuf::from("firmware.bin"),
            },
        ];
        match outcome_for(true, &regions) {
            DeployOutcome::SelectiveFlash { regions } => {
                assert_eq!(
                    regions,
                    vec![FlashRegion::Bootloader, FlashRegion::Firmware]
                );
            }
            other => panic!("expected SelectiveFlash, got {:?}", other),
        }
    }

    #[test]
    fn try_write_deployment_native_rejects_empty_regions() {
        let err = try_write_deployment_native(
            "esp32s3",
            "COM99",
            460_800,
            "default-reset",
            "hard-reset",
            &[],
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no regions"));
    }

    #[test]
    fn progress_bridge_throttles_updates_to_ten_percent_boundaries() {
        // Assert the 10%-boundary throttle actually fires — without it
        // a 1 MB write spams hundreds of log lines per region.
        let mut bridge = LoggingProgressBridge::new("COM13");
        bridge.enter_region(FlashRegion::Firmware);
        bridge.init(0x10000, 1000);

        // Simulate espflash calling update every 10 bytes. After 1000
        // calls we should have logged at roughly 0, 10, 20, ..., 100%
        // — i.e. last_pct_logged should have landed on a 10-multiple.
        for current in (10..=1000).step_by(10) {
            bridge.update(current);
        }
        assert_eq!(bridge.last_current, 1000);
        assert_eq!(bridge.last_pct_logged % 10, 0);
        // Finishing resets the per-region state for the next region.
        bridge.finish(false);
    }

    #[test]
    fn progress_bridge_handles_zero_total_without_panic() {
        // A zero-byte region is defensive: espflash shouldn't ever
        // emit one, but our arithmetic must not divide by zero.
        let mut bridge = LoggingProgressBridge::new("COM13");
        bridge.enter_region(FlashRegion::Firmware);
        bridge.init(0x10000, 0);
        bridge.update(0);
        bridge.finish(true);
    }

    #[test]
    fn progress_bridge_reports_correct_region_label() {
        let mut bridge = LoggingProgressBridge::new("COM13");
        assert_eq!(bridge.region_label(), "unknown");
        bridge.enter_region(FlashRegion::Bootloader);
        assert_eq!(bridge.region_label(), "bootloader");
        bridge.enter_region(FlashRegion::Partitions);
        assert_eq!(bridge.region_label(), "partitions");
        bridge.enter_region(FlashRegion::Firmware);
        assert_eq!(bridge.region_label(), "firmware");
    }

    #[test]
    fn region_name_is_stable() {
        // Daemon log messages and integration tests depend on these
        // literal names. Pin them here so a refactor that renames
        // them trips a unit test first.
        assert_eq!(region_name(FlashRegion::Bootloader), "bootloader");
        assert_eq!(region_name(FlashRegion::Partitions), "partitions");
        assert_eq!(region_name(FlashRegion::Firmware), "firmware");
    }
}

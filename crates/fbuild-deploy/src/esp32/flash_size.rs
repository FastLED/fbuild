//! Declared-vs-detected flash-size preflight for `fbuild deploy`
//! (FastLED/fbuild#1992).
//!
//! `fbuild deploy` used to pass the board JSON's `upload.flash_size` straight
//! to `esptool write-flash --flash-size`, which patches the image header. The
//! board JSON describes the board *family*, not the module actually attached
//! (`esp32-c6-devkitc-1` declares 8 MB; the ESP32-C6FH4 module on the desk is
//! a 4 MB part, i.e. `esp32-c6-devkitm-1`). Nothing checked the two against
//! each other, so the write verified, fbuild printed `deploy succeeded`, and
//! the app then panicked at boot:
//!
//! ```text
//! E (142) spi_flash: Detected size(4096k) smaller than the size in the binary
//!   image header(8192k). Probe failed.
//! assert failed: __esp_system_init_fn_init_flash startup_funcs.c_id:118
//! ```
//!
//! esptool itself only warns (`Set flash_size 8MB is larger than the available
//! flash size of 4MB`) and exits 0. This module asks the chip
//! (`esptool flash-id`) before writing and refuses the flash with an
//! actionable message instead of producing a device that cannot boot.
//!
//! The probe is best-effort: an unparseable or failed `flash-id` leaves the
//! deploy unchanged (`Unknown`), because a working deploy must never be
//! blocked by a diagnostic. `FBUILD_SKIP_FLASH_SIZE_PREFLIGHT=1` skips it
//! outright.

use std::time::Duration;

use fbuild_core::Result;
use fbuild_core::subprocess::run_command;

use super::parse::{parse_detected_flash_size, parse_flash_size_bytes};

/// Budget for the `esptool flash-id` probe. A stub upload plus a JEDEC read is
/// sub-second over USB-CDC (0.65 s measured on an ESP32-C6FH4); the budget
/// only bounds a wedged serial stack, mirroring `DEFAULT_FLASH_TIMEOUT`.
pub(super) const FLASH_ID_TIMEOUT: Duration = Duration::from_secs(20);

/// Outcome of comparing the declared image-header size with the chip's own
/// flash size.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FlashSizeVerdict {
    /// The declared size fits the chip (equal, or smaller than it).
    Fits,
    /// The image header declares more flash than the chip has: booting it
    /// trips the `spi_flash` probe assert.
    Exceeds { declared: u64, detected: u64 },
    /// One side is unparseable/absent, so no verdict is possible.
    Unknown,
}

pub(super) fn flash_size_verdict(declared: Option<u64>, detected: Option<u64>) -> FlashSizeVerdict {
    match (declared, detected) {
        (Some(declared), Some(detected)) if declared > detected => {
            FlashSizeVerdict::Exceeds { declared, detected }
        }
        (Some(_), Some(_)) => FlashSizeVerdict::Fits,
        _ => FlashSizeVerdict::Unknown,
    }
}

/// Render a byte count the way the board JSONs spell it (`4MB`, `512KB`).
pub(super) fn describe_flash_size(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    const KB: u64 = 1024;
    if bytes >= MB {
        format!("{}MB", bytes / MB)
    } else {
        format!("{}KB", bytes / KB)
    }
}

/// The refusal. It names both numbers, the board-file field to change, and the
/// boot failure being prevented, because the fix is a one-line config edit the
/// user cannot infer from esptool's warning alone.
pub(super) fn mismatch_message(chip: &str, declared: u64, detected: u64) -> String {
    format!(
        "the {chip} on this port reports {} of flash, but the build targets {}. \
         Flashing it would produce a device that panics at boot \
         (`spi_flash: Detected size(Nk) smaller than the size in the binary image header(Mk)`, \
         then `assert failed: __esp_system_init_fn_init_flash`). \
         Set `board_upload.flash_size = {}` in platformio.ini (or select the board JSON that \
         matches the attached module) and rebuild before deploying. \
         Set FBUILD_SKIP_FLASH_SIZE_PREFLIGHT=1 to flash anyway.",
        describe_flash_size(detected),
        describe_flash_size(declared),
        describe_flash_size(detected)
    )
}

fn skip_requested() -> bool {
    std::env::var_os("FBUILD_SKIP_FLASH_SIZE_PREFLIGHT").is_some()
}

/// Build the `esptool flash-id` argv. Pure so the layout is unit-testable
/// without hardware; the leading element is the resolved esptool exactly like
/// the write/verify argv builders.
pub(super) fn build_flash_id_args(esptool: &[String], chip: &str, port: &str) -> Vec<String> {
    let mut args = esptool.to_vec();
    args.extend([
        "--chip".to_string(),
        chip.to_string(),
        "--port".to_string(),
        port.to_string(),
        "flash-id".to_string(),
    ]);
    args
}

async fn probe_detected_flash_size(
    esptool: &[String],
    chip: &str,
    port: &str,
    caller_path: Option<&str>,
    timeout: Duration,
) -> Option<u64> {
    if esptool.is_empty() {
        return None;
    }
    let args = build_flash_id_args(esptool, chip, port);
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let env_overlay = fbuild_core::subprocess::bare_name_path_overlay(&args[0], caller_path);
    let result = run_command(&args_ref, None, env_overlay.as_deref(), Some(timeout))
        .await
        .ok()?;
    if !result.success() {
        return None;
    }
    parse_detected_flash_size(&result.stdout).or_else(|| parse_detected_flash_size(&result.stderr))
}

/// Refuse a flash whose image header declares more flash than the chip has.
///
/// Called before every ESP32 write path (native and esptool). `Err` means the
/// image cannot boot on this part, so nothing is written.
pub(super) async fn ensure_flash_size_fits(
    esptool: &[String],
    chip: &str,
    port: &str,
    flash_size_label: &str,
    caller_path: Option<&str>,
    deploy_timeout: Duration,
) -> Result<()> {
    if skip_requested() {
        tracing::info!("flash-size preflight skipped (FBUILD_SKIP_FLASH_SIZE_PREFLIGHT)");
        return Ok(());
    }
    // `esptool write-flash --flash-size <label>` patches the image header from
    // this label, so the label is what the chip will be asked to boot.
    let declared = parse_flash_size_bytes(flash_size_label).ok();
    // Never let the diagnostic outlast the flash it is guarding: a wedged
    // serial stack should still reach the flash's own containment budget.
    let probe_budget = FLASH_ID_TIMEOUT.min(deploy_timeout);
    let detected = probe_detected_flash_size(esptool, chip, port, caller_path, probe_budget).await;
    match flash_size_verdict(declared, detected) {
        FlashSizeVerdict::Exceeds {
            declared: declared_bytes,
            detected: detected_bytes,
        } => Err(fbuild_core::FbuildError::DeployFailed(mismatch_message(
            chip,
            declared_bytes,
            detected_bytes,
        ))),
        FlashSizeVerdict::Fits => Ok(()),
        FlashSizeVerdict::Unknown => {
            tracing::warn!(
                port,
                "flash-id probe gave no flash size; skipping the declared-vs-detected flash check"
            );
            Ok(())
        }
    }
}

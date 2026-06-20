//! Pre-deploy artifact size validation against board flash region.
//!
//! FastLED/fbuild#690. Run before every deploy backend is invoked so
//! a too-large firmware fails fast at fbuild time instead of mid-flash
//! when half the bytes have already been written.
//!
//! ## What this catches
//!
//! esptool does its own size check on the ESP side, so the gap was
//! invisible there. Everywhere else (LPC8xx, Teensy, STM32, RP2040)
//! the silent path is: build a too-large binary → backend tool writes
//! the bytes that fit → device boots into garbage → "looks dead." The
//! FastLED/FastLED#3300 LPC845-BRK bring-up sat next to this exact
//! failure mode but didn't trip it (firmware was 53.85 KB / 64 KB,
//! ~84% — one PR away).
//!
//! ## What this won't catch
//!
//! Per-region overflow on boards with split flash regions (LPC845
//! CRP zone, RP2040 XIP boot region) — the linker map already
//! catches those at link time. This check is the "total bytes vs
//! total flash" net; the linker map is the per-region net.

use fbuild_core::{FbuildError, Result};
use std::path::Path;

/// Soft warning threshold — warn the user when artifact size crosses
/// this fraction of the board's flash ceiling. FastLED's LPC845-BRK
/// bring-up firmware sat at 84% per FastLED/FastLED#3339; emitting
/// the warning at 90% gives ~6% headroom before the hard ceiling
/// bites.
pub const WARN_THRESHOLD_PERCENT: f64 = 90.0;

/// Validate a firmware artifact's on-disk size against the board's
/// flash region.
///
/// - `Ok(())` — fits. Warns via `tracing::warn!` when size exceeds
///   [`WARN_THRESHOLD_PERCENT`] of `max_size`.
/// - `Err(FbuildError::FirmwareTooLarge { … })` — exceeds the
///   board's flash region. Carries `actual`, `max`, and `board`
///   so a CI / `bash autoresearch` failure can pinpoint the
///   overflow without parsing strings.
///
/// `max_size = None` (e.g. ESP32 catch-all boards without an
/// explicit `upload.maximum_size`) skips the check with a debug log
/// — esptool's own size check still runs at deploy time and is the
/// authoritative source for the ESP path.
///
/// I/O errors from the size read propagate via [`FbuildError::Io`].
pub fn check_artifact_fits_flash(
    artifact: &Path,
    max_size: Option<u64>,
    board_name: &str,
) -> Result<()> {
    let Some(max) = max_size else {
        tracing::debug!(
            board = board_name,
            artifact = %artifact.display(),
            "skip pre-deploy size check: board has no maximum_size \
             (e.g. esp32 catch-all — esptool's own check runs at deploy)"
        );
        return Ok(());
    };

    let actual = std::fs::metadata(artifact).map_err(FbuildError::Io)?.len();

    let percent_used = if max == 0 {
        // Defensive: a board JSON with max=0 is malformed; treat any
        // artifact as overflow to surface the config bug.
        100.0
    } else {
        (actual as f64 / max as f64) * 100.0
    };

    if actual > max {
        return Err(FbuildError::FirmwareTooLarge {
            board: board_name.to_string(),
            actual,
            max,
            percent_used,
        });
    }

    if percent_used >= WARN_THRESHOLD_PERCENT {
        tracing::warn!(
            board = board_name,
            actual,
            max,
            percent_used,
            artifact = %artifact.display(),
            "firmware at {percent_used:.1}% of board flash — approaching ceiling"
        );
    } else {
        tracing::debug!(
            board = board_name,
            actual,
            max,
            percent_used,
            "artifact fits flash"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_artifact(temp: &tempfile::TempDir, bytes: usize) -> std::path::PathBuf {
        let path = temp.path().join("firmware.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&vec![0u8; bytes]).unwrap();
        path
    }

    #[test]
    fn artifact_under_flash_size_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = make_artifact(&tmp, 1024);
        check_artifact_fits_flash(&artifact, Some(65_536), "test_board").unwrap();
    }

    #[test]
    fn artifact_exactly_at_flash_size_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = make_artifact(&tmp, 65_536);
        check_artifact_fits_flash(&artifact, Some(65_536), "test_board").unwrap();
    }

    #[test]
    fn artifact_one_byte_over_overflows() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = make_artifact(&tmp, 65_537);
        let err = check_artifact_fits_flash(&artifact, Some(65_536), "lpc845brk").unwrap_err();
        match err {
            FbuildError::FirmwareTooLarge {
                board,
                actual,
                max,
                percent_used,
            } => {
                assert_eq!(board, "lpc845brk");
                assert_eq!(actual, 65_537);
                assert_eq!(max, 65_536);
                assert!(percent_used > 100.0);
            }
            other => panic!("expected FirmwareTooLarge, got {other:?}"),
        }
    }

    /// LPC845-BRK row: real-world numbers from FastLED/FastLED#3339.
    /// 53.85 KB of 64 KB → 84% — under the warn threshold, fine.
    #[test]
    fn lpc845_fastled_bringup_firmware_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let actual_bytes: usize = (53.85 * 1024.0) as usize;
        let max_bytes: u64 = 64 * 1024;
        let artifact = make_artifact(&tmp, actual_bytes);
        check_artifact_fits_flash(&artifact, Some(max_bytes), "lpc845brk").unwrap();
    }

    /// LPC845-BRK at 92% — should succeed but warn.
    #[test]
    fn lpc845_at_92_percent_passes_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let actual_bytes: usize = (0.92 * 65_536.0) as usize;
        let max_bytes: u64 = 65_536;
        let artifact = make_artifact(&tmp, actual_bytes);
        check_artifact_fits_flash(&artifact, Some(max_bytes), "lpc845brk").unwrap();
    }

    /// LPC804: 32 KB flash. 33 KB firmware → overflow.
    #[test]
    fn lpc804_overflow_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = make_artifact(&tmp, 33_792);
        let err = check_artifact_fits_flash(&artifact, Some(32_768), "lpc804").unwrap_err();
        assert!(matches!(err, FbuildError::FirmwareTooLarge { .. }));
    }

    /// Teensy 4.0: 2 MB flash. 1 KB firmware → far under, no warning.
    #[test]
    fn teensy40_small_firmware_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = make_artifact(&tmp, 1024);
        check_artifact_fits_flash(&artifact, Some(2 * 1024 * 1024), "teensy40").unwrap();
    }

    /// RP2040 Pico: 2 MB flash. 100 KB firmware → fine.
    #[test]
    fn rp2040_pico_firmware_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = make_artifact(&tmp, 100 * 1024);
        check_artifact_fits_flash(&artifact, Some(2 * 1024 * 1024), "pico").unwrap();
    }

    /// ESP32 (catch-all without maximum_size): skip the check.
    #[test]
    fn esp32_catchall_without_max_size_skips_check() {
        let tmp = tempfile::tempdir().unwrap();
        // Even a 100-byte artifact passes when max_size is None —
        // esptool runs its own check at deploy time.
        let artifact = make_artifact(&tmp, 100);
        check_artifact_fits_flash(&artifact, None, "esp32dev").unwrap();
    }

    #[test]
    fn missing_artifact_surfaces_io_error() {
        let missing = std::path::Path::new("/tmp/this/does/not/exist");
        let err = check_artifact_fits_flash(missing, Some(1024), "anywhere").unwrap_err();
        assert!(matches!(err, FbuildError::Io(_)));
    }

    /// Defensive: a malformed board JSON with `maximum_size: 0` must
    /// surface as overflow (any non-zero artifact triggers), not
    /// silently divide-by-zero.
    #[test]
    fn zero_max_size_treats_any_artifact_as_overflow() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = make_artifact(&tmp, 1);
        let err = check_artifact_fits_flash(&artifact, Some(0), "broken_board").unwrap_err();
        assert!(matches!(err, FbuildError::FirmwareTooLarge { .. }));
    }
}

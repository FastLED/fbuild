//! Verify-flash region types and stdout parser.

use super::parse::parse_hex_offset;

/// Which of the three logical flash regions a verify/write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashRegion {
    Bootloader,
    Partitions,
    Firmware,
}

/// Per-region outcome parsed from `esptool verify-flash` stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionVerifyResult {
    pub region: FlashRegion,
    pub matched: bool,
}

/// Result of a `try_verify_deployment` call.
#[derive(Debug, Clone)]
pub enum VerifyOutcome {
    /// All flashed regions match the candidate image; flashing would be
    /// a no-op. The device has been hard-reset by esptool's
    /// `--after hard-reset` so it's already running the requested image.
    Match { stdout: String, stderr: String },
    /// At least one region differs from the local files. `regions` carries
    /// the parsed per-region verdict when stdout was understood; empty
    /// when parsing failed and the caller must flash everything.
    Mismatch {
        stdout: String,
        stderr: String,
        regions: Vec<RegionVerifyResult>,
    },
}

impl VerifyOutcome {
    /// Convenience: returns `true` only for `Match`.
    pub fn is_match(&self) -> bool {
        matches!(self, VerifyOutcome::Match { .. })
    }
}

/// Parse `esptool verify-flash` stdout into per-region results.
///
/// esptool 5.x emits, for each region:
///
/// ```text
/// Verifying 0x6060 (24672) bytes at 0x00000000 in flash against 'bootloader.bin'...
/// Verification successful (digest matched).
/// ```
///
/// or, on mismatch, `Verification failed (digest mismatch).`. We map the
/// `at 0x{addr:#010x}` line to one of the three known offsets, then read
/// the next non-blank line as the verdict. Unknown addresses are skipped.
/// Returns an empty `Vec` when nothing could be parsed — callers must then
/// fall back to flashing all regions.
pub fn parse_verify_regions(
    stdout: &str,
    bootloader_offset: &str,
    partitions_offset: &str,
    firmware_offset: &str,
) -> Vec<RegionVerifyResult> {
    let boot_addr = parse_hex_offset(bootloader_offset).ok();
    let parts_addr = parse_hex_offset(partitions_offset).ok();
    let fw_addr = parse_hex_offset(firmware_offset).ok();

    let mut results: Vec<RegionVerifyResult> = Vec::new();
    let mut pending: Option<FlashRegion> = None;

    for raw in stdout.lines() {
        let line = raw.trim();
        if let Some(addr) = extract_verifying_at_address(line) {
            pending = if Some(addr) == boot_addr {
                Some(FlashRegion::Bootloader)
            } else if Some(addr) == parts_addr {
                Some(FlashRegion::Partitions)
            } else if Some(addr) == fw_addr {
                Some(FlashRegion::Firmware)
            } else {
                None
            };
            continue;
        }
        if let Some(region) = pending {
            if line.starts_with("Verification successful") {
                if !results.iter().any(|r| r.region == region) {
                    results.push(RegionVerifyResult {
                        region,
                        matched: true,
                    });
                }
                pending = None;
            } else if line.starts_with("Verification failed") {
                if !results.iter().any(|r| r.region == region) {
                    results.push(RegionVerifyResult {
                        region,
                        matched: false,
                    });
                }
                pending = None;
            }
        }
    }
    results
}

/// Extract the `0x...` address from an esptool `Verifying ... at 0xNNNNNNNN in flash ...` line.
fn extract_verifying_at_address(line: &str) -> Option<u64> {
    if !line.starts_with("Verifying ") {
        return None;
    }
    let marker = " at 0x";
    let start = line.find(marker)? + marker.len();
    let tail = &line[start..];
    let end = tail
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(tail.len());
    u64::from_str_radix(&tail[..end], 16).ok()
}

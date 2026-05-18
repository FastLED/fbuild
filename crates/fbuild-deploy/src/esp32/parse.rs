//! Hex offset and flash-size parsers shared across the esp32 module.

use fbuild_core::Result;

pub(super) fn parse_hex_offset(raw: &str) -> Result<u64> {
    let trimmed = raw.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(trimmed, 16).map_err(|e| {
        fbuild_core::FbuildError::DeployFailed(format!("invalid flash offset '{}': {}", raw, e))
    })
}

/// Parse a hex flash offset (accepts `0x` prefix) as a `u32`. espflash's
/// `FLASH_MD5SUM` command takes 32-bit offsets, so we narrow the shared
/// [`parse_hex_offset`] result here. Only used by the native verify/write
/// path, so gated with the crate's feature to avoid dead-code lints on
/// default builds.
#[cfg(feature = "espflash-native")]
pub(super) fn parse_hex_offset_u32(raw: &str) -> Result<u32> {
    let as_u64 = parse_hex_offset(raw)?;
    u32::try_from(as_u64).map_err(|_| {
        fbuild_core::FbuildError::DeployFailed(format!(
            "native verify: flash offset {} does not fit in u32",
            raw
        ))
    })
}

pub(super) fn parse_flash_size_bytes(raw: &str) -> Result<u64> {
    let upper = raw.trim().to_ascii_uppercase();
    if let Some(num) = upper.strip_suffix("MB") {
        return num
            .trim()
            .parse::<u64>()
            .map(|n| n * 1024 * 1024)
            .map_err(|e| {
                fbuild_core::FbuildError::DeployFailed(format!(
                    "invalid flash size '{}': {}",
                    raw, e
                ))
            });
    }
    if let Some(num) = upper.strip_suffix("KB") {
        return num.trim().parse::<u64>().map(|n| n * 1024).map_err(|e| {
            fbuild_core::FbuildError::DeployFailed(format!("invalid flash size '{}': {}", raw, e))
        });
    }
    Err(fbuild_core::FbuildError::DeployFailed(format!(
        "unsupported flash size label '{}'",
        raw
    )))
}

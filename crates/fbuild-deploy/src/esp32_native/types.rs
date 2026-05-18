//! Shared region types used by the native verify and write paths.
//!
//! Kept as a distinct module so a future change (e.g. encrypted-write
//! flags per-region) doesn't silently leak between paths.

use crate::esp32::FlashRegion;

/// A single region (flash offset + local firmware file) to verify.
#[derive(Debug, Clone)]
pub struct NativeVerifyRegion {
    pub region: FlashRegion,
    pub offset: u32,
    pub path: std::path::PathBuf,
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

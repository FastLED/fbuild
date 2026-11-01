//! RP2040 half of `fbuild uploadfs` (FastLED/fbuild#1358).
//!
//! arduino-pico has no partition CSV: the firmware mounts the region its
//! `boards.txt` `flash` menu option declares through `FS_START`/`FS_END`, and
//! `fbuild buildfs` packs the image with those same bounds. Flashing that image
//! is not an `esptool` region write — it is a UF2 transfer into the mounted ROM
//! BOOTSEL volume, because the ROM loader is what programs QSPI at an absolute
//! flash address.
//!
//! So this half:
//!
//! * encodes the image as UF2 blocks addressed at the planned `fs_start`, the
//!   address the firmware itself will mount (no ESP8266-style mapping removal);
//! * requires exactly one mounted Pico volume and refuses an ambiguous set
//!   rather than flashing an arbitrary board;
//! * treats the ROM ejecting the volume as the write confirmation, and refuses
//!   `--verify`, because a region cannot be read back without PICOBOOT and
//!   silently accepting the flag would claim a proof we cannot produce.

use std::path::Path;

use fbuild_core::{FbuildError, Result};

use super::buildfs::ImagePlan;

/// What an RP2040 filesystem flash did.
#[derive(Debug)]
pub(super) struct RegionFlash {
    /// The mounted BOOTSEL volume the image was written into.
    pub(super) volume: String,
    /// Number of 512-byte UF2 blocks written (256 bytes of image each).
    pub(super) blocks: usize,
    /// The `NEW.UF2` path the ROM consumed.
    pub(super) destination: String,
}

/// The absolute flash address the region image starts at.
///
/// No translation: arduino-pico's `build.fs_start` is already the XIP address
/// the firmware mounts, and the ROM loader programs flash at exactly that
/// address. Subtracting the `0x10000000` mapping here would relocate the
/// filesystem out from under the firmware.
pub(super) fn uf2_base_address(region_offset: u64) -> u64 {
    region_offset
}

/// `--verify` has no RP2040 equivalent: the ROM BOOTSEL handshake confirms the
/// write by ejecting the volume, and reading the region back needs PICOBOOT.
/// Refusing is honest; accepting the flag while reporting `verified: false`
/// would look like a failed verification.
pub(super) fn require_verify_supported(verify: bool) -> Result<()> {
    if verify {
        return Err(FbuildError::ConfigError(
            "`--verify` is not available for an RP2040 filesystem flash: the write is confirmed \
             only by the ROM BOOTSEL volume ejecting itself, and the region cannot be read back \
             without PICOBOOT"
                .to_string(),
        ));
    }
    Ok(())
}

/// Pick the BOOTSEL volume to write: the only one, or a clear error naming the
/// choices. More than one candidate is ambiguous — writing both would flash the
/// wrong board.
pub(super) fn choose_volume(volumes: &[String]) -> Result<String> {
    match volumes {
        [only] => Ok(only.clone()),
        [] => Err(FbuildError::DeployFailed(
            "no Pico BOOTSEL volume detected; hold BOOTSEL while plugging the board in \
             (or connect it and retry once it mounts)"
                .to_string(),
        )),
        many => Err(FbuildError::DeployFailed(format!(
            "{} Pico BOOTSEL volumes detected ({}); unplug all but the board to flash",
            many.len(),
            many.join(", ")
        ))),
    }
}

/// Encode `image` for the planned region and write it to the single mounted
/// BOOTSEL volume.
pub(super) fn flash(plan: &ImagePlan, image: &Path) -> Result<RegionFlash> {
    let image_bytes = std::fs::read(image).map_err(|error| {
        FbuildError::DeployFailed(format!("failed to read {}: {error}", image.display()))
    })?;
    let uf2 = fbuild_deploy::rp2040::encode_uf2_for_region(
        &image_bytes,
        uf2_base_address(plan.partition.offset),
    )?;
    let blocks = uf2.len() / 512;
    let volumes: Vec<String> = fbuild_deploy::rp2040::bootsel_volumes()
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let volume = choose_volume(&volumes)?;
    let destination = fbuild_deploy::rp2040::write_uf2_to_volume(&uf2, Path::new(&volume))?;
    Ok(RegionFlash {
        volume,
        blocks,
        destination: destination.display().to_string(),
    })
}

#[cfg(test)]
#[path = "uploadfs_rp2040_tests.rs"]
mod tests;

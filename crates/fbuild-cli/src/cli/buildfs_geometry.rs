//! Block/page geometry of a flash filesystem (FastLED/fbuild#1358).
//!
//! The image tool must format the volume with the *same* block and page size
//! the firmware mounts it with, otherwise a perfectly-sized image still fails
//! to mount. So the geometry is never a constant of this crate: each platform
//! resolver reads it from the declaration the build itself uses — the ESP32
//! builder's 4096/256, or an ESP8266 `eagle.flash.*.ld`'s `_FS_block` /
//! `_FS_page` symbols.

use std::path::Path;

use super::buildfs::StagedFile;

/// Block size, page size and LittleFS metadata reservation of one volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FsGeometry {
    /// Erase/allocation block (`-b`).
    pub(super) block: u64,
    /// Page, the smallest addressable unit inside a block (`-p`).
    pub(super) page: u64,
    /// Metadata pairs LittleFS reserves at the head of the volume, in blocks.
    pub(super) metadata_blocks: u64,
}

impl FsGeometry {
    /// arduino-esp32 formats both LittleFS and SPIFFS with 4096-byte blocks and
    /// 256-byte pages.
    pub(crate) const ESP32: Self = Self {
        block: 4096,
        page: 256,
        metadata_blocks: 2,
    };

    pub(crate) fn from_parts(block: u64, page: u64) -> Self {
        Self {
            block,
            page,
            metadata_blocks: Self::ESP32.metadata_blocks,
        }
    }

    fn align_up(value: u64, align: u64) -> u64 {
        value.div_ceil(align) * align
    }

    /// Conservative size of the resulting image under this geometry: every
    /// file occupies whole pages, each path segment costs a small directory
    /// entry, and LittleFS keeps its metadata blocks at the head of the volume.
    ///
    /// The authoritative check is still the image tool's own `-s` bound.
    pub(crate) fn estimate_bytes(&self, files: &[StagedFile]) -> u64 {
        let data: u64 = files
            .iter()
            .map(|f| Self::align_up(f.bytes.max(1), self.page))
            .sum();
        let directory: u64 = files
            .iter()
            .map(|f| f.relative.split('/').count() as u64 * self.page)
            .sum();
        data + directory + self.metadata_blocks * self.block
    }

    /// `mklittlefs`/`mkspiffs` argv (same `-c/-b/-p/-s <image>` contract).
    pub(crate) fn tool_args(&self, staging: &Path, image_size: u64, image: &Path) -> Vec<String> {
        vec![
            "-c".to_string(),
            staging.display().to_string(),
            "-b".to_string(),
            self.block.to_string(),
            "-p".to_string(),
            self.page.to_string(),
            "-s".to_string(),
            image_size.to_string(),
            image.display().to_string(),
        ]
    }
}

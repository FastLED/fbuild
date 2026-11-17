//! ESP32 firmware size reporting from ELF section headers.
//!
//! The Berkeley `size` format lumps `.flash.rodata` (flash-resident) into the
//! `data` column alongside `.dram0.data`, which once reported 602% RAM on an
//! ESP32-C6 (FastLED/fbuild#1261). The replacement parsed `size -A` by section
//! name, but counted every `.dram0.*` section as RAM: the S3/C3 linker scripts'
//! `.dram0.dummy` is a `SHT_NOBITS` placeholder for the IRAM half of the shared
//! DIRAM region, and it landed in RAM *and* in flash (FastLED/fbuild#2013).
//!
//! This module reads the section headers instead (shared reader:
//! [`crate::elf_size::read_elf_section_sizes`]) and classifies:
//!
//! - **Flash** = every allocated section with contents, exactly the generic
//!   rule of [`SizeInfo::from_elf_sections`] (FastLED/fbuild#2012): flash code
//!   and rodata plus the initialised RAM images `elf2image` copies from flash
//!   (`.iram0.*`, `.dram0.data`, `.rtc.*`) and `.eh_frame`. `SHT_NOBITS`
//!   placeholders (`.dram0.dummy`, `.iram0.text_end`) carry no bytes.
//! - **RAM** = static DRAM: [`DRAM_SECTIONS`], i.e. PlatformIO espressif32's
//!   `SIZEDATAREGEXP` set. IRAM code is not RAM here, as in PlatformIO and in
//!   the previous fbuild report (`idf_size` lists it as a separate IRAM row).
//!
//! Both are [`SizeInfo::from_elf_sections_with_ram`] with that set; the
//! ESP32-only part is rejecting an ELF without ESP32 sections.

use std::path::Path;

use fbuild_core::{ElfSectionSize, SizeInfo};

/// Bump whenever the classification changes, so cached reports computed by an
/// older classifier are not served for an unchanged ELF. Version 2 matched
/// `BUILD_FINGERPRINT_VERSION`, which the caches used before #2013.
pub(crate) const SIZE_REPORT_VERSION: u32 = 3;

/// The static DRAM sections of an ESP32 image (PlatformIO espressif32's
/// `SIZEDATAREGEXP`). Matched by exact name: `.dram0.dummy` is deliberately
/// absent (FastLED/fbuild#2013).
const DRAM_SECTIONS: &[&str] = &[".dram0.data", ".dram0.bss", ".noinit"];

/// Read `elf_path`'s section headers and classify them for an ESP32 target.
///
/// Errors when the file is not ELF or has no ESP32 (`.flash.*` / `.dram0.*`)
/// sections.
pub(crate) fn esp32_report_size(
    elf_path: &Path,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
) -> fbuild_core::Result<SizeInfo> {
    let sections = crate::elf_size::read_elf_section_sizes(elf_path)?;
    esp32_size_from_sections(&sections, max_flash, max_ram).ok_or_else(|| {
        fbuild_core::FbuildError::BuildFailed(format!(
            "size report: {} has no ESP32 .flash.*/.dram0.* sections",
            elf_path.display()
        ))
    })
}

/// Classify ESP32 ELF sections (see the module docs). Returns `None` when no
/// ESP32-prefixed allocated section is present.
pub(crate) fn esp32_size_from_sections(
    sections: &[ElfSectionSize],
    max_flash: Option<u64>,
    max_ram: Option<u64>,
) -> Option<SizeInfo> {
    let is_esp =
        |s: &ElfSectionSize| s.name.starts_with(".flash.") || s.name.starts_with(".dram0.");
    if !sections.iter().any(|s| s.alloc && is_esp(s)) {
        return None;
    }
    Some(SizeInfo::from_elf_sections_with_ram(
        sections,
        DRAM_SECTIONS,
        max_flash,
        max_ram,
    ))
}

#[cfg(test)]
#[path = "size_report_tests.rs"]
mod tests;

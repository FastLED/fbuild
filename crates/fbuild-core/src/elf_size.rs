//! ELF section-header size accounting (FastLED/fbuild#2012).
//!
//! The firmware size summary is derived from section headers rather than
//! the toolchain `size` tool, whose Berkeley format counts read-only
//! `SHT_NOBITS` reservations (Arduino-Pico's `.heap`) as flash.

use crate::SizeInfo;

/// The section-header facts size accounting needs for one ELF section
/// (FastLED/fbuild#2012). Built from `SHF_ALLOC`, `sh_type != SHT_NOBITS`,
/// `SHF_WRITE` and `SHF_EXECINSTR`, plus the section name for platforms
/// whose layout needs it (ESP32's `.dram0.*`, FastLED/fbuild#2013).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfSectionSize {
    /// Section name from `.shstrtab`; empty when it cannot be read.
    pub name: String,
    pub size: u64,
    /// `SHF_ALLOC`: occupies target memory at run time.
    pub alloc: bool,
    /// Not `SHT_NOBITS`: the section carries bytes in the firmware image.
    pub has_contents: bool,
    /// `SHF_WRITE`.
    pub writable: bool,
    /// `SHF_EXECINSTR`.
    pub executable: bool,
}

impl SizeInfo {
    /// Classify ELF sections into `text` / `data` / `bss` from their headers.
    ///
    /// Follows GNU `size`'s Berkeley rules (code or read-only → `text`,
    /// writable with contents → `data`, writable `SHT_NOBITS` → `bss`) with
    /// one correction (FastLED/fbuild#2012): a `SHT_NOBITS` section has no
    /// bytes in the image, so it is never `text`. Berkeley files a read-only
    /// `NOLOAD` reservation such as Arduino-Pico's `.heap` under `text`,
    /// inflating the flash figure by the whole heap. Such a read-only
    /// reservation is not static data either, so it is skipped; RAM stays
    /// `data + bss` as before. `total_flash` is therefore exactly the bytes
    /// of every allocated section with contents, i.e. the `firmware.bin`
    /// payload.
    pub fn from_elf_sections(
        sections: &[ElfSectionSize],
        max_flash: Option<u64>,
        max_ram: Option<u64>,
    ) -> Self {
        let (mut text, mut data, mut bss) = (0u64, 0u64, 0u64);
        for s in sections.iter().filter(|s| s.alloc) {
            let bucket = if !s.has_contents {
                if !s.writable {
                    continue;
                }
                &mut bss
            } else if s.executable || !s.writable {
                &mut text
            } else {
                &mut data
            };
            *bucket = bucket.saturating_add(s.size);
        }
        Self {
            text,
            data,
            bss,
            total_flash: text.saturating_add(data),
            total_ram: data.saturating_add(bss),
            max_flash,
            max_ram,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(size, alloc, has_contents, writable, executable)` → section row.
    fn sec(size: u64, alloc: bool, contents: bool, w: bool, x: bool) -> ElfSectionSize {
        ElfSectionSize {
            name: String::new(),
            size,
            alloc,
            has_contents: contents,
            writable: w,
            executable: x,
        }
    }

    /// FastLED/fbuild#2012: the allocated sections of an empty Arduino-Pico
    /// sketch. `.heap` is NOBITS but read-only, which GNU `size`'s Berkeley
    /// format files under `text`; it must not count as flash.
    fn rp2040_empty_sketch_sections() -> Vec<ElfSectionSize> {
        vec![
            sec(256, true, true, false, true),       // .boot2
            sec(10_228, true, true, true, false),    // .ota
            sec(1_804, true, true, true, false),     // .partition
            sec(61_584, true, true, false, true),    // .text
            sec(3_872, true, true, false, false),    // .rodata
            sec(192, true, false, true, false),      // .ram_vector_table
            sec(4_648, true, true, false, true),     // .data (AX: holds RAM code)
            sec(5_088, true, false, true, false),    // .bss
            sec(252_216, true, false, false, false), // .heap (NOBITS, read-only)
            sec(2_048, true, false, true, false),    // .stack1_dummy
            sec(9_999, false, true, false, false),   // .debug_info (not ALLOC)
        ]
    }

    #[test]
    fn size_info_from_elf_sections_excludes_nobits_from_flash() {
        let info = SizeInfo::from_elf_sections(
            &rp2040_empty_sketch_sections(),
            Some(2_097_152),
            Some(270_336),
        );
        // Every ALLOC section with contents, i.e. exactly firmware.bin.
        assert_eq!(info.total_flash, 82_392);
        assert_eq!(info.text, 256 + 61_584 + 3_872 + 4_648);
        assert_eq!(info.data, 10_228 + 1_804);
        // Writable NOBITS only; the read-only `.heap` reservation is neither
        // flash nor static RAM.
        assert_eq!(info.bss, 192 + 5_088 + 2_048);
        assert_eq!(info.total_ram, 19_360);
        assert_eq!(info.max_flash, Some(2_097_152));
        assert_eq!(info.max_ram, Some(270_336));
    }

    #[test]
    fn size_info_from_elf_sections_matches_berkeley_for_classic_layout() {
        // AVR-style: .text RX, .data RW (contents), .bss RW NOBITS.
        let sections = [
            sec(924, true, true, false, true),
            sec(14, true, true, true, false),
            sec(9, true, false, true, false),
        ];
        let info = SizeInfo::from_elf_sections(&sections, None, None);
        assert_eq!((info.text, info.data, info.bss), (924, 14, 9));
        assert_eq!(info.total_flash, 938);
        assert_eq!(info.total_ram, 23);
    }
}

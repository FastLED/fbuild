//! ELF section-header size accounting (FastLED/fbuild#2012).
//!
//! The firmware size summary is derived from section headers rather than
//! the toolchain `size` tool, whose Berkeley format counts read-only
//! `SHT_NOBITS` reservations (Arduino-Pico's `.heap`) as flash.

use crate::SizeInfo;

/// PlatformIO's default static-RAM section set (FastLED/fbuild#2027).
///
/// Every PlatformIO ARM/AVR builder fbuild mirrors (atmelavr, atmelsam,
/// nordicnrf51, nordicnrf52, raspberrypi, renesas-ra, siliconlabsefm32,
/// ststm32, nxplpc) computes RAM with
/// `SIZEDATAREGEXP = ^(?:\.data|\.bss|\.noinit)\s+(\d+)` over `size -A`,
/// i.e. these exact section names. Heap and stack reservations (`.heap`,
/// `._user_heap_stack`, `.stack_dummy`, ...) are therefore not static RAM;
/// the linker script's own `> RAM` region and `ASSERT`s still reject a
/// layout whose reservations do not fit.
pub const PLATFORMIO_RAM_SECTIONS: &[&str] = &[".data", ".bss", ".noinit"];

/// The section-header facts size accounting needs for one ELF section
/// (FastLED/fbuild#2012). Built from `SHF_ALLOC`, `sh_type != SHT_NOBITS`,
/// `SHF_WRITE` and `SHF_EXECINSTR`, plus the section name, which selects
/// the RAM sections (FastLED/fbuild#2013, #2027).
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
    /// Classify ELF sections with PlatformIO's default RAM set,
    /// [`PLATFORMIO_RAM_SECTIONS`]. See [`Self::from_elf_sections_with_ram`].
    pub fn from_elf_sections(
        sections: &[ElfSectionSize],
        max_flash: Option<u64>,
        max_ram: Option<u64>,
    ) -> Self {
        Self::from_elf_sections_with_ram(sections, PLATFORMIO_RAM_SECTIONS, max_flash, max_ram)
    }

    /// Classify ELF sections into flash and static RAM from their headers.
    ///
    /// - **Flash** (`total_flash`) is every allocated section with contents,
    ///   i.e. exactly the `firmware.bin` payload. `SHT_NOBITS` reservations
    ///   carry no image bytes, so they are never flash (FastLED/fbuild#2012:
    ///   GNU `size`'s Berkeley format files a read-only `NOLOAD` `.heap`
    ///   under `text`).
    /// - **RAM** (`total_ram`) is the allocated sections named in
    ///   `ram_sections`, PlatformIO's `SIZEDATAREGEXP` set for the platform,
    ///   matched by exact name (FastLED/fbuild#2027). Section flags cannot
    ///   tell static data from a heap/stack reservation (both are writable
    ///   `SHT_NOBITS`), nor see that Arduino-Pico's `AX` `.data` is in RAM
    ///   while its writable `.ota` lives in flash.
    ///
    /// `data` is the RAM sections with contents (loaded from flash, so they
    /// count in both totals), `bss` the RAM sections without, and `text` the
    /// rest of the image.
    pub fn from_elf_sections_with_ram(
        sections: &[ElfSectionSize],
        ram_sections: &[&str],
        max_flash: Option<u64>,
        max_ram: Option<u64>,
    ) -> Self {
        let (mut image, mut data, mut bss) = (0u64, 0u64, 0u64);
        for s in sections.iter().filter(|s| s.alloc) {
            if s.has_contents {
                image = image.saturating_add(s.size);
            }
            if ram_sections.contains(&s.name.as_str()) {
                let bucket = if s.has_contents { &mut data } else { &mut bss };
                *bucket = bucket.saturating_add(s.size);
            }
        }
        Self {
            text: image.saturating_sub(data),
            data,
            bss,
            total_flash: image,
            total_ram: data.saturating_add(bss),
            max_flash,
            max_ram,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(size, alloc, has_contents, writable, executable)` → unnamed row.
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

    /// Allocated section row with a name: `(name, size, contents, w, x)`.
    fn named(name: &str, size: u64, contents: bool, w: bool, x: bool) -> ElfSectionSize {
        ElfSectionSize {
            name: name.to_string(),
            ..sec(size, true, contents, w, x)
        }
    }

    /// FastLED/fbuild#2012: the allocated sections of an empty Arduino-Pico
    /// sketch. `.heap` is NOBITS but read-only, which GNU `size`'s Berkeley
    /// format files under `text`; it must not count as flash.
    fn rp2040_empty_sketch_sections() -> Vec<ElfSectionSize> {
        vec![
            named(".boot2", 256, true, false, true),
            named(".ota", 10_228, true, true, false),
            named(".partition", 1_804, true, true, false),
            named(".text", 61_584, true, false, true),
            named(".rodata", 3_872, true, false, false),
            named(".ram_vector_table", 192, false, true, false),
            named(".data", 4_648, true, false, true), // AX: holds RAM code
            named(".bss", 5_088, false, true, false),
            named(".heap", 252_216, false, false, false), // NOBITS, read-only
            named(".stack1_dummy", 2_048, false, true, false),
            ElfSectionSize {
                name: ".debug_info".to_string(),
                ..sec(9_999, false, true, false, false) // not ALLOC
            },
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
        assert_eq!(info.text + info.data, info.total_flash);
        assert_eq!(info.max_flash, Some(2_097_152));
        assert_eq!(info.max_ram, Some(270_336));
    }

    /// FastLED/fbuild#2027 / #2041: Arduino-Pico RAM is PlatformIO
    /// raspberrypi's `.data + .bss + .noinit`. `.data` counts although it is
    /// flagged `AX` (it holds RAM code); the writable flash sections `.ota` /
    /// `.partition` and the reservations `.ram_vector_table` /
    /// `.stack1_dummy` do not. Was 19_360 under the flag rule.
    #[test]
    fn rp2040_ram_is_platformio_data_bss_noinit() {
        let info = SizeInfo::from_elf_sections(&rp2040_empty_sketch_sections(), None, None);
        assert_eq!((info.data, info.bss), (4_648, 5_088));
        assert_eq!(info.total_ram, 9_736);
    }

    /// FastLED/fbuild#2027: Adafruit nRF52 (`nrf52_common.ld`) grows a
    /// writable `.heap` up to `__StackLimit`. Static RAM is PlatformIO
    /// nordicnrf52's `.data + .bss + .noinit`, not the heap reservation.
    /// Section sizes are the issue's FastLED (16 LEDs) build.
    #[test]
    fn nrf52_ram_excludes_writable_heap_reservation() {
        let sections = [
            named(".text", 120_000, true, false, true),
            named(".ARM.exidx", 8, true, false, false),
            named(".data", 820, true, true, false),
            named(".bss", 17_844, false, true, false),
            named(".heap", 216_856, false, true, false),
        ];
        let info = SizeInfo::from_elf_sections(&sections, Some(815_104), Some(237_568));
        // Was 235_520 under the flag rule.
        assert_eq!(info.total_ram, 18_664);
        assert_eq!((info.data, info.bss), (820, 17_844));
        assert_eq!(info.total_flash, 120_000 + 8 + 820);
        assert_eq!(info.text + info.data, info.total_flash);
    }

    /// STM32duino (real `stm32f103c8` ELF): `._user_heap_stack` reserves
    /// `_Min_Heap_Size + _Min_Stack_Size`. PlatformIO ststm32 leaves it out
    /// of RAM; `ld` still fails the link when it does not fit (`> RAM`).
    #[test]
    fn stm32_ram_excludes_user_heap_stack() {
        let sections = [
            named(".isr_vector", 268, true, false, false),
            named(".text", 20_768, true, false, true),
            named(".rodata", 2_596, true, false, false),
            named(".ARM", 8, true, false, false),
            named(".preinit_array", 0, true, true, false),
            named(".init_array", 12, true, false, false),
            named(".fini_array", 4, true, false, false),
            named(".data", 248, true, true, false),
            named(".bss", 1_744, false, true, false),
            named("._user_heap_stack", 1_536, false, true, false),
        ];
        let info = SizeInfo::from_elf_sections(&sections, None, Some(20_480));
        // Was 3_528 (data + bss + the 1_536-byte reservation).
        assert_eq!(info.total_ram, 248 + 1_744);
        assert_eq!(info.total_flash, 23_904);
    }

    /// Arduino Renesas (`fsp.ld`): `.heap`, `.stack_dummy`, `.vector_table`
    /// (which runs to the end of RAM) and the DTC table are reservations.
    #[test]
    fn renesas_ram_excludes_heap_stack_and_vector_reservations() {
        let sections = [
            named(".text", 40_000, true, false, true),
            named(".fsp_dtc_vector_table", 128, false, true, false),
            named(".data", 300, true, true, false),
            named(".noinit", 40, false, true, false),
            named(".bss", 2_000, false, true, false),
            named(".heap", 4_096, false, true, false),
            named(".stack_dummy", 1_024, false, true, false),
            named(".vector_table", 256, false, true, false),
        ];
        let info = SizeInfo::from_elf_sections(&sections, None, None);
        assert_eq!(info.total_ram, 300 + 40 + 2_000);
        assert_eq!((info.data, info.bss), (300, 2_040));
    }

    /// AVR (real `uno` ELF plus a `.noinit`): `.data + .bss + .noinit` was
    /// already the whole figure, so the RAM that feeds AVR's fatal overflow
    /// check is unchanged.
    #[test]
    fn avr_ram_is_unchanged() {
        let sections = [
            named(".data", 64, true, true, false),
            named(".text", 5_104, true, false, true),
            named(".bss", 435, false, true, false),
            named(".noinit", 16, false, true, false),
        ];
        let info = SizeInfo::from_elf_sections(&sections, Some(32_256), Some(2_048));
        assert_eq!(info.total_ram, 64 + 435 + 16);
        assert_eq!(info.total_flash, 64 + 5_104);
    }

    /// AVR `EEMEM` lands in `.eeprom` (writable, with contents, at 0x810000).
    /// It is not SRAM, and PlatformIO atmelavr does not count it as RAM.
    #[test]
    fn avr_eeprom_is_not_ram() {
        let sections = [
            named(".data", 64, true, true, false),
            named(".text", 5_104, true, false, true),
            named(".bss", 435, false, true, false),
            named(".eeprom", 32, true, true, false),
        ];
        let info = SizeInfo::from_elf_sections(&sections, None, None);
        assert_eq!(info.total_ram, 64 + 435);
    }

    /// A platform whose PlatformIO builder has its own `SIZEDATAREGEXP`
    /// passes that set; names match exactly, as the anchored regex does.
    #[test]
    fn explicit_ram_section_set_is_matched_by_exact_name() {
        let sections = [
            named(".text", 1_000, true, false, true),
            named(".data", 10, true, true, false),
            named(".data.extra", 20, true, true, false),
            named(".bss", 30, false, true, false),
            named(".stack", 256, false, true, false),
        ];
        let ram = [".data", ".bss", ".stack"];
        let info = SizeInfo::from_elf_sections_with_ram(&sections, &ram, None, None);
        assert_eq!(info.total_ram, 10 + 30 + 256);
        assert_eq!(info.total_flash, 1_030);
        assert_eq!(info.text, 1_020);
    }

    /// Unallocated sections never count, even with a RAM name.
    #[test]
    fn unallocated_ram_named_section_is_ignored() {
        let sections = [ElfSectionSize {
            name: ".bss".to_string(),
            ..sec(64, false, false, true, false)
        }];
        let info = SizeInfo::from_elf_sections(&sections, None, None);
        assert_eq!(info.total_ram, 0);
    }

    #[test]
    fn size_info_from_elf_sections_matches_berkeley_for_classic_layout() {
        // AVR-style: .text RX, .data RW (contents), .bss RW NOBITS.
        let sections = [
            named(".text", 924, true, false, true),
            named(".data", 14, true, true, false),
            named(".bss", 9, false, true, false),
        ];
        let info = SizeInfo::from_elf_sections(&sections, None, None);
        assert_eq!((info.text, info.data, info.bss), (924, 14, 9));
        assert_eq!(info.total_flash, 938);
        assert_eq!(info.total_ram, 23);
    }
}

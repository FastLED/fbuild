//! ELF section-header reader for the firmware size summary
//! (FastLED/fbuild#2012). [`crate::linker::LinkerBase::report_size`] feeds
//! these rows to [`fbuild_core::SizeInfo::from_elf_sections`].

use std::path::Path;

use fbuild_core::{ElfSectionSize, FbuildError, Result, SizeInfo};

/// Size report for a platform whose PlatformIO builder counts RAM over its
/// own section set instead of [`fbuild_core::PLATFORMIO_RAM_SECTIONS`]
/// (FastLED/fbuild#2027). `ram_sections` mirrors that builder's
/// `SIZEDATAREGEXP`, matched by exact section name.
pub fn report_size_with_ram(
    elf_path: &Path,
    ram_sections: &[&str],
    max_flash: Option<u64>,
    max_ram: Option<u64>,
) -> Result<SizeInfo> {
    let sections = read_elf_section_sizes(elf_path)?;
    Ok(SizeInfo::from_elf_sections_with_ram(
        &sections,
        ram_sections,
        max_flash,
        max_ram,
    ))
}

/// Read the size-relevant header facts of every section in an ELF file
/// (FastLED/fbuild#2012). Errors when the file is unreadable or not ELF.
pub fn read_elf_section_sizes(elf_path: &Path) -> Result<Vec<ElfSectionSize>> {
    use object::read::elf::{ElfFile32, ElfFile64, FileHeader, SectionHeader};
    use object::{Endianness, FileKind};

    fn collect<Elf: FileHeader<Endian = Endianness>>(
        elf: &object::read::elf::ElfFile<'_, Elf>,
    ) -> Vec<ElfSectionSize> {
        let endian = elf.endian();
        let table = elf.elf_section_table();
        table
            .iter()
            .map(|sh| {
                let flags: u64 = sh.sh_flags(endian).into();
                let name = table
                    .section_name(endian, sh)
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .unwrap_or_default();
                ElfSectionSize {
                    name,
                    size: sh.sh_size(endian).into(),
                    alloc: flags & u64::from(object::elf::SHF_ALLOC) != 0,
                    has_contents: sh.sh_type(endian) != object::elf::SHT_NOBITS,
                    writable: flags & u64::from(object::elf::SHF_WRITE) != 0,
                    executable: flags & u64::from(object::elf::SHF_EXECINSTR) != 0,
                }
            })
            .collect()
    }

    let fail = |what: String| {
        FbuildError::BuildFailed(format!("size report: {what} for {}", elf_path.display()))
    };
    let bytes = std::fs::read(elf_path).map_err(|e| fail(format!("could not read ELF ({e})")))?;
    match FileKind::parse(&bytes[..]).map_err(|e| fail(format!("unknown file kind ({e})")))? {
        FileKind::Elf32 => ElfFile32::<Endianness>::parse(&bytes[..])
            .map(|elf| collect(&elf))
            .map_err(|e| fail(format!("ELF32 parse failed ({e})"))),
        FileKind::Elf64 => ElfFile64::<Endianness>::parse(&bytes[..])
            .map(|elf| collect(&elf))
            .map_err(|e| fail(format!("ELF64 parse failed ({e})"))),
        other => Err(fail(format!("expected ELF, got {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::linker::LinkerBase;

    /// An Arduino-Pico-shaped ELF (FastLED/fbuild#2012): flash code and
    /// rodata, AX `.data`, writable `.bss`, and a read-only `NOLOAD` `.heap`.
    fn write_pico_like_elf(path: &Path) {
        use object::write::Object;
        use object::{Architecture, BinaryFormat, Endianness, SectionFlags, SectionKind};
        let mut obj = Object::new(BinaryFormat::Elf, Architecture::Arm, Endianness::Little);
        let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
        obj.append_section_data(text, &[0u8; 0x100], 4);
        let rodata = obj.add_section(Vec::new(), b".rodata".to_vec(), SectionKind::ReadOnlyData);
        obj.append_section_data(rodata, &[0u8; 0x30], 4);
        let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
        obj.append_section_data(data, &[0u8; 0x10], 4);
        let bss = obj.add_section(Vec::new(), b".bss".to_vec(), SectionKind::UninitializedData);
        obj.append_section_bss(bss, 0x40, 4);
        let heap = obj.add_section(
            Vec::new(),
            b".heap".to_vec(),
            SectionKind::UninitializedData,
        );
        obj.append_section_bss(heap, 0x10_000, 4);
        obj.section_mut(heap).flags = SectionFlags::Elf {
            sh_flags: u64::from(object::elf::SHF_ALLOC),
        };
        let dbg = obj.add_section(Vec::new(), b".debug_info".to_vec(), SectionKind::Debug);
        obj.append_section_data(dbg, &[0u8; 0x80], 1);
        std::fs::write(path, obj.write().unwrap()).unwrap();
    }

    #[test]
    fn report_size_does_not_count_read_only_nobits_heap_as_flash() {
        let tmp = tempfile::tempdir().unwrap();
        let elf = tmp.path().join("firmware.elf");
        write_pico_like_elf(&elf);
        let info = LinkerBase::report_size(&elf, Some(0x20_0000), Some(0x4_2000)).unwrap();
        // Only sections with image bytes: .text + .rodata + .data.
        assert_eq!(info.total_flash, 0x100 + 0x30 + 0x10);
        assert_eq!(info.total_ram, 0x10 + 0x40);
        assert_eq!(info.max_flash, Some(0x20_0000));
    }

    /// An Adafruit-nRF52-shaped ELF (FastLED/fbuild#2027): `.data`, `.bss`
    /// and a *writable* NOBITS `.heap` that fills the RAM the stack leaves.
    fn write_nrf52_like_elf(path: &Path) {
        use object::write::Object;
        use object::{Architecture, BinaryFormat, Endianness, SectionKind};
        let mut obj = Object::new(BinaryFormat::Elf, Architecture::Arm, Endianness::Little);
        let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
        obj.append_section_data(text, &[0u8; 0x200], 4);
        let data = obj.add_section(Vec::new(), b".data".to_vec(), SectionKind::Data);
        obj.append_section_data(data, &[0u8; 820], 4);
        for (name, size) in [(&b".bss"[..], 17_844), (&b".heap"[..], 216_856)] {
            let id = obj.add_section(Vec::new(), name.to_vec(), SectionKind::UninitializedData);
            obj.append_section_bss(id, size, 4);
        }
        std::fs::write(path, obj.write().unwrap()).unwrap();
    }

    #[test]
    fn report_size_excludes_writable_heap_reservation_from_ram() {
        let tmp = tempfile::tempdir().unwrap();
        let elf = tmp.path().join("firmware.elf");
        write_nrf52_like_elf(&elf);
        let heap = super::read_elf_section_sizes(&elf)
            .unwrap()
            .into_iter()
            .find(|s| s.name == ".heap")
            .unwrap();
        assert!(heap.alloc && heap.writable && !heap.has_contents);
        let info = LinkerBase::report_size(&elf, None, Some(237_568)).unwrap();
        // Static RAM only (.data + .bss), not the 216_856-byte heap.
        assert_eq!(info.total_ram, 18_664);
        assert_eq!(info.total_flash, 0x200 + 820);
    }

    #[test]
    fn report_size_with_ram_uses_the_platform_section_set() {
        let tmp = tempfile::tempdir().unwrap();
        let elf = tmp.path().join("firmware.elf");
        write_nrf52_like_elf(&elf);
        let info = super::report_size_with_ram(&elf, &[".bss", ".heap"], None, None).unwrap();
        assert_eq!(info.total_ram, 17_844 + 216_856);
    }

    #[test]
    fn read_elf_section_sizes_reports_section_names() {
        let tmp = tempfile::tempdir().unwrap();
        let elf = tmp.path().join("firmware.elf");
        write_pico_like_elf(&elf);
        let sections = super::read_elf_section_sizes(&elf).unwrap();
        let heap = sections.iter().find(|s| s.name == ".heap").unwrap();
        assert_eq!(heap.size, 0x10_000);
        assert!(heap.alloc && !heap.has_contents && !heap.writable);
        assert!(
            sections
                .iter()
                .any(|s| s.name == ".text" && s.size == 0x100)
        );
    }

    #[test]
    fn report_size_rejects_non_elf() {
        let tmp = tempfile::tempdir().unwrap();
        let bogus = tmp.path().join("firmware.elf");
        std::fs::write(&bogus, b"not an elf").unwrap();
        assert!(LinkerBase::report_size(&bogus, None, None).is_err());
    }
}

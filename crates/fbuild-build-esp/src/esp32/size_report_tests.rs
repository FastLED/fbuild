use super::*;

/// Section flags as `readelf -S` prints them for the ESP32 images below.
#[derive(Clone, Copy)]
enum Kind {
    /// `PROGBITS AX`: code.
    Code,
    /// `PROGBITS A`: read-only data.
    Rodata,
    /// `PROGBITS WA`: initialised data (ESP-IDF's `.flash.rodata` is WA too).
    Data,
    /// `NOBITS WA`: zero-initialised data or a placeholder.
    NoBits,
    /// Not `SHF_ALLOC` (debug info, comments).
    NoAlloc,
}

fn sec(name: &str, size: u64, kind: Kind) -> ElfSectionSize {
    let (alloc, has_contents, writable, executable) = match kind {
        Kind::Code => (true, true, false, true),
        Kind::Rodata => (true, true, false, false),
        Kind::Data => (true, true, true, false),
        Kind::NoBits => (true, false, true, false),
        Kind::NoAlloc => (false, true, false, false),
    };
    ElfSectionSize {
        name: name.to_string(),
        size,
        alloc,
        has_contents,
        writable,
        executable,
    }
}

/// FastLED/fbuild#2013: the allocated sections of an empty Arduino sketch for
/// `esp32-s3-devkitc-1`, copied from the issue's table.
fn esp32s3_empty_sketch() -> Vec<ElfSectionSize> {
    vec![
        sec(".rtc.force_slow", 32, Kind::Data),
        sec(".iram0.vectors", 1_028, Kind::Code),
        sec(".iram0.text", 56_939, Kind::Code),
        sec(".dram0.dummy", 41_728, Kind::NoBits),
        sec(".dram0.data", 14_468, Kind::Data),
        sec(".dram0.bss", 6_968, Kind::NoBits),
        sec(".flash.text", 133_624, Kind::Code),
        sec(".flash.appdesc", 256, Kind::Rodata),
        sec(".flash.rodata", 71_684, Kind::Data),
        sec(".iram0.text_end", 145, Kind::NoBits),
        sec(".debug_info", 999_999, Kind::NoAlloc),
    ]
}

#[test]
fn esp32s3_excludes_dram0_dummy_from_ram_and_flash() {
    let info =
        esp32_size_from_sections(&esp32s3_empty_sketch(), Some(8 << 20), Some(327_680)).unwrap();
    // Static DRAM only: .dram0.data + .dram0.bss (the issue's 21,436).
    assert_eq!(info.data, 14_468);
    assert_eq!(info.bss, 6_968);
    assert_eq!(info.total_ram, 21_436);
    // Every allocated section with contents: within firmware.bin's 278,144
    // (the rest is image headers, padding and the checksum).
    assert_eq!(
        info.total_flash,
        32 + 1_028 + 56_939 + 14_468 + 133_624 + 256 + 71_684
    );
    assert!(278_144 - info.total_flash < 256);
    assert_eq!(info.text + info.data, info.total_flash);
    assert_eq!(info.max_flash, Some(8 << 20));
    assert_eq!(info.max_ram, Some(327_680));
}

/// FastLED/fbuild#2013: `readelf -S` of an empty `esp32-c3-devkitm-1` sketch
/// (Arduino-ESP32 3.3.12, ESP-IDF 5.5.5), allocated sections only. RISC-V
/// images keep `.eh_frame` (11,044 bytes) in flash; the old report dropped it.
/// `firmware.bin` was 272,416 bytes.
fn esp32c3_empty_sketch() -> Vec<ElfSectionSize> {
    vec![
        sec(".rtc.force_slow", 32, Kind::Data),
        sec(".rtc_reserved", 40, Kind::NoBits),
        sec(".iram0.text", 47_352, Kind::Code),
        sec(".iram0.text_end", 264, Kind::NoBits),
        sec(".dram0.dummy", 47_616, Kind::NoBits),
        sec(".dram0.data", 7_062, Kind::Data),
        sec(".noinit", 2, Kind::NoBits),
        sec(".dram0.bss", 6_256, Kind::NoBits),
        sec(".flash.text", 153_254, Kind::Code),
        sec(".flash_rodata_dummy", 196_608, Kind::NoBits),
        sec(".flash.appdesc", 256, Kind::Rodata),
        sec(".flash.rodata", 53_020, Kind::Rodata),
        sec(".flash.init_array", 272, Kind::Data),
        sec(".eh_frame", 11_044, Kind::Rodata),
        sec(".riscv.attributes", 92, Kind::NoAlloc),
    ]
}

#[test]
fn esp32c3_counts_eh_frame_and_excludes_dram0_dummy() {
    let info = esp32_size_from_sections(&esp32c3_empty_sketch(), None, None).unwrap();
    assert_eq!(info.total_ram, 7_062 + 2 + 6_256);
    assert_eq!(info.total_flash, 272_292);
    assert!(272_416 - info.total_flash < 256, "within firmware.bin");
}

/// Classic ESP32 has no `.dram0.dummy`, and its RAM figure was already right;
/// it must stay so. Same build as above for `esp32dev`; `firmware.bin` was
/// 261,632 bytes, while the old report said 258,165 because it dropped
/// `.eh_frame` and `.rtc.force_slow`.
#[test]
fn classic_esp32_ram_is_dram0_data_plus_bss() {
    let sections = vec![
        sec(".rtc.force_slow", 32, Kind::Data),
        sec(".rtc_fast_reserved", 16, Kind::NoBits),
        sec(".rtc_slow_reserved", 24, Kind::NoBits),
        sec(".iram0.vectors", 1_028, Kind::Code),
        sec(".iram0.text", 59_147, Kind::Code),
        sec(".dram0.data", 16_865, Kind::Data),
        sec(".noinit", 3, Kind::NoBits),
        sec(".dram0.bss", 5_376, Kind::NoBits),
        sec(".flash.appdesc", 256, Kind::Rodata),
        sec(".flash.rodata", 53_456, Kind::Data),
        sec(".eh_frame", 3_316, Kind::Rodata),
        sec(".flash.text", 127_412, Kind::Code),
        sec(".iram0.text_end", 1, Kind::NoBits),
    ];
    let info = esp32_size_from_sections(&sections, None, None).unwrap();
    assert_eq!((info.data, info.bss), (16_865, 3 + 5_376));
    assert_eq!(info.total_ram, 22_244);
    assert_eq!(info.total_flash, 261_512);
    assert!(261_632 - info.total_flash < 256, "within firmware.bin");
}

#[test]
fn noinit_counts_as_static_ram() {
    let sections = vec![
        sec(".dram0.data", 100, Kind::Data),
        sec(".noinit", 40, Kind::NoBits),
        sec(".dram0.bss", 200, Kind::NoBits),
        sec(".flash.text", 1_000, Kind::Code),
    ];
    let info = esp32_size_from_sections(&sections, None, None).unwrap();
    assert_eq!((info.data, info.bss), (100, 240));
    assert_eq!(info.total_flash, 1_100);
}

#[test]
fn returns_none_without_esp32_sections() {
    let sections = vec![
        sec(".text", 1_000, Kind::Code),
        sec(".data", 10, Kind::Data),
        sec(".bss", 20, Kind::NoBits),
    ];
    assert!(esp32_size_from_sections(&sections, None, None).is_none());
}

/// End to end through the shared header reader: a real ELF file whose
/// `.dram0.dummy` is `SHT_NOBITS`.
#[test]
fn esp32_report_size_reads_elf_headers() {
    use object::write::Object;
    use object::{Architecture, BinaryFormat, Endianness, SectionKind};

    let mut obj = Object::new(BinaryFormat::Elf, Architecture::Riscv32, Endianness::Little);
    let mut add = |name: &str, kind: SectionKind, len: u64| {
        let id = obj.add_section(Vec::new(), name.as_bytes().to_vec(), kind);
        if kind == SectionKind::UninitializedData {
            obj.append_section_bss(id, len, 4);
        } else {
            obj.append_section_data(id, &vec![0u8; len as usize], 4);
        }
    };
    add(".iram0.text", SectionKind::Text, 0x200);
    add(".dram0.dummy", SectionKind::UninitializedData, 0x8000);
    add(".dram0.data", SectionKind::Data, 0x40);
    add(".dram0.bss", SectionKind::UninitializedData, 0x80);
    add(".flash.text", SectionKind::Text, 0x1000);
    add(".flash.rodata", SectionKind::ReadOnlyData, 0x300);

    let tmp = tempfile::tempdir().unwrap();
    let elf = tmp.path().join("firmware.elf");
    std::fs::write(&elf, obj.write().unwrap()).unwrap();

    let info = esp32_report_size(&elf, Some(0x40_0000), None).unwrap();
    assert_eq!(info.total_ram, 0x40 + 0x80);
    assert_eq!(info.total_flash, 0x200 + 0x40 + 0x1000 + 0x300);
    assert_eq!(info.max_flash, Some(0x40_0000));

    std::fs::write(&elf, b"not an elf").unwrap();
    assert!(esp32_report_size(&elf, None, None).is_err());
}

/// FastLED/fbuild#2042: an ESP32 layout without `.dram0.data`, `.dram0.bss`
/// or `.noinit` still reports (the link succeeded) but warns that RAM reads
/// 0 because no RAM section was found, not because none is used.
#[tracing_test::traced_test]
#[test]
fn esp32_report_size_warns_without_dram_sections() {
    use object::write::Object;
    use object::{Architecture, BinaryFormat, Endianness, SectionKind};

    let mut obj = Object::new(BinaryFormat::Elf, Architecture::Riscv32, Endianness::Little);
    let text = obj.add_section(Vec::new(), b".flash.text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0u8; 0x100], 4);
    let dummy = obj.add_section(
        Vec::new(),
        b".dram0.dummy".to_vec(),
        SectionKind::UninitializedData,
    );
    obj.append_section_bss(dummy, 0x8000, 4);

    let tmp = tempfile::tempdir().unwrap();
    let elf = tmp.path().join("firmware.elf");
    std::fs::write(&elf, obj.write().unwrap()).unwrap();

    let info = esp32_report_size(&elf, None, Some(0x5_0000)).unwrap();
    assert_eq!(info.total_ram, 0);
    assert!(logs_contain(
        "no allocated RAM section (.dram0.data, .dram0.bss, .noinit)"
    ));
}

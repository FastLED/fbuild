//! Unit tests for `symbol_analysis`. Split out from `mod.rs` so the
//! production module stays under the workspace's 1000-LOC gate.

use super::*;

#[test]
fn parse_nm_line_minimal() {
    let row = parse_nm_line("42008de8 00001251 T fl::Channel::showPixels()").unwrap();
    assert_eq!(row.0, 0x42008de8);
    assert_eq!(row.1, 0x1251);
    assert_eq!(row.2, 'T');
    assert_eq!(row.3, "fl::Channel::showPixels()");
}

#[test]
fn parse_nm_line_skips_unsized() {
    assert!(parse_nm_line("       U _impure_ptr").is_none());
}

#[test]
fn extract_archive_from_pio_path() {
    let (arc, obj) =
        extract_archive_and_object(".pio/build/esp32s3/lib0d9/libFastLED.a(fl.channels+.cpp.o)");
    assert_eq!(arc.as_deref(), Some("libFastLED.a"));
    assert_eq!(obj, "fl.channels+.cpp.o");
}

#[test]
fn extract_bare_object_no_archive() {
    let (arc, obj) = extract_archive_and_object(".pio/build/esp32s3/src/main.cpp.o");
    assert!(arc.is_none());
    assert_eq!(obj, "main.cpp.o");
}

#[test]
fn parse_linker_map_combined_line() {
    // Simulates one output section with two input section rows.
    let text = "\
Linker script and memory map

.flash.text     0x42000020    0x4026c
 .text.foo      0x42000020       0x10 path/libFastLED.a(foo.cpp.o)
 .text.bar      0x42000030       0x20 path/src/main.cpp.o
";
    let ranges = parse_linker_map(text);
    assert_eq!(ranges.len(), 2);
    assert_eq!(ranges[0].addr, 0x42000020);
    assert_eq!(ranges[0].size, 0x10);
    assert_eq!(ranges[0].output_section, ".flash.text");
    assert_eq!(ranges[0].archive.as_deref(), Some("libFastLED.a"));
    assert_eq!(ranges[0].object, "foo.cpp.o");
    assert!(ranges[1].archive.is_none());
    assert_eq!(ranges[1].object, "main.cpp.o");
}

#[test]
fn parse_linker_map_section_on_own_line() {
    // Many sections wrap because their mangled names are too long for
    // one row.
    let text = "\
Linker script and memory map

.flash.text     0x42000020    0x4026c
 .text._ZN2fl7Channel10showPixelsERS_
                0x42000020       0x40 path/libFastLED.a(fl.channels.cpp.o)
";
    let ranges = parse_linker_map(text);
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].size, 0x40);
    assert_eq!(
        ranges[0].input_section,
        ".text._ZN2fl7Channel10showPixelsERS_"
    );
    assert_eq!(ranges[0].archive.as_deref(), Some("libFastLED.a"));
}

#[test]
fn parse_linker_map_drops_tombstones() {
    // Tombstone: 0x00000000 address means linker dropped the section.
    let text = "\
Linker script and memory map

.flash.text     0x42000020    0x4026c
 .text.dead     0x00000000       0x40 path/libFastLED.a(dead.cpp.o)
 .text.live     0x42000020       0x40 path/libFastLED.a(live.cpp.o)
";
    let ranges = parse_linker_map(text);
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].input_section, ".text.live");
}

#[test]
fn input_section_index_lookup_inside_range() {
    let ranges = vec![
        InputSectionRange {
            addr: 0x1000,
            size: 0x10,
            output_section: ".flash.text".into(),
            input_section: ".text.foo".into(),
            archive: Some("libA.a".into()),
            object: "foo.o".into(),
        },
        InputSectionRange {
            addr: 0x2000,
            size: 0x20,
            output_section: ".flash.text".into(),
            input_section: ".text.bar".into(),
            archive: Some("libB.a".into()),
            object: "bar.o".into(),
        },
    ];
    let idx = InputSectionIndex::build(ranges);
    assert_eq!(idx.lookup(0x1000).unwrap().input_section, ".text.foo");
    assert_eq!(idx.lookup(0x1008).unwrap().input_section, ".text.foo");
    assert!(idx.lookup(0x1010).is_none()); // past end of first range
    assert_eq!(idx.lookup(0x2010).unwrap().input_section, ".text.bar");
    assert!(idx.lookup(0x500).is_none()); // before any range
}

#[test]
fn build_fine_grained_map_attributes_symbols() {
    let ranges = vec![InputSectionRange {
        addr: 0x42000020,
        size: 0x40,
        output_section: ".flash.text".into(),
        input_section: ".text.show".into(),
        archive: Some("libFastLED.a".into()),
        object: "fl.channels.cpp.o".into(),
    }];
    let nm = vec![(
        0x42000020u64,
        0x40u64,
        'T',
        "_ZN2fl7Channel10showPixelsERS_".to_string(),
    )];
    let demangled = vec!["fl::Channel::showPixels(fl::Channel&)".to_string()];
    let map = build_fine_grained_map(
        "fw.elf".into(),
        Some("fw.map".into()),
        nm,
        demangled,
        ranges,
    );
    assert_eq!(map.symbols.len(), 1);
    let sym = &map.symbols[0];
    assert_eq!(sym.archive.as_deref(), Some("libFastLED.a"));
    assert_eq!(sym.output_section.as_deref(), Some(".flash.text"));
    assert_eq!(sym.demangled, "fl::Channel::showPixels(fl::Channel&)");
    assert_eq!(sym.source, "nm");
    assert_eq!(map.total_flash, 0x40);
    assert_eq!(map.total_ram, 0);
}

// ---------------- Issue #425: map-derived owner extraction ----------------

#[test]
fn extract_owner_text() {
    assert_eq!(
        extract_owner_from_section(".text._ZN2fl7Channel10showPixelsEv").as_deref(),
        Some("_ZN2fl7Channel10showPixelsEv")
    );
}

#[test]
fn extract_owner_rodata_bare() {
    assert_eq!(
        extract_owner_from_section(".rodata._ZN2fl5audio8detector4VibeC1Ev").as_deref(),
        Some("_ZN2fl5audio8detector4VibeC1Ev")
    );
}

#[test]
fn extract_owner_rodata_str1() {
    // The dominant case for the FL_WARN/FL_LOG string pool that
    // motivated this issue: per-function string fragments.
    assert_eq!(
        extract_owner_from_section(".rodata._ZNK2fl5audio8detector4Vibe7getNameEv.str1.1")
            .as_deref(),
        Some("_ZNK2fl5audio8detector4Vibe7getNameEv")
    );
    assert_eq!(
        extract_owner_from_section(".rodata.foo.str1.8").as_deref(),
        Some("foo")
    );
}

#[test]
fn extract_owner_rodata_cst() {
    // Constant pool fragments (cst4 = 4-byte aligned, cst16 = 16-byte aligned).
    assert_eq!(
        extract_owner_from_section(".rodata.foo.cst4").as_deref(),
        Some("foo")
    );
    assert_eq!(
        extract_owner_from_section(".rodata.foo.cst16").as_deref(),
        Some("foo")
    );
}

#[test]
fn extract_owner_literal_xtensa() {
    // Xtensa literal pools attached to a function.
    assert_eq!(
        extract_owner_from_section(".literal._ZN2fl7ChannelC2Ev").as_deref(),
        Some("_ZN2fl7ChannelC2Ev")
    );
}

#[test]
fn extract_owner_data_and_bss() {
    assert_eq!(
        extract_owner_from_section(".data.g_state").as_deref(),
        Some("g_state")
    );
    assert_eq!(
        extract_owner_from_section(".bss.g_buf").as_deref(),
        Some("g_buf")
    );
}

#[test]
fn extract_owner_gnu_linkonce_and_rel_ro() {
    // COMDAT (older GCC) and vtable relocation-read-only sections.
    assert_eq!(
        extract_owner_from_section(".gnu.linkonce.t._ZN3foo3barEv").as_deref(),
        Some("_ZN3foo3barEv")
    );
    assert_eq!(
        extract_owner_from_section(".gnu.linkonce.r._ZTV3foo").as_deref(),
        Some("_ZTV3foo")
    );
    assert_eq!(
        extract_owner_from_section(".data.rel.ro._ZTV3foo").as_deref(),
        Some("_ZTV3foo")
    );
}

#[test]
fn extract_owner_returns_none_for_unknown_shapes() {
    // Catch-all `.rodata` (no per-symbol granularity).
    assert!(extract_owner_from_section(".rodata").is_none());
    // Anonymous filler.
    assert!(extract_owner_from_section(".comment").is_none());
    // Bare `.text` (compiled without -ffunction-sections).
    assert!(extract_owner_from_section(".text").is_none());
    // Suffix-only would also be empty.
    assert!(extract_owner_from_section(".rodata.").is_none());
}

#[test]
fn extract_owner_str1_with_non_digit_tail_is_owner() {
    // Don't over-strip: a function named `foo.str1.bar` (legal,
    // rare) should not have its tail trimmed since "bar" isn't
    // digits.
    assert_eq!(
        extract_owner_from_section(".rodata.foo.str1.bar").as_deref(),
        Some("foo.str1.bar")
    );
}

#[test]
fn build_fine_grained_map_synthesises_rodata_pool() {
    // Two ranges in libFastLED.a:
    //   - .text.foo at 0x42000020 (nm-covered)
    //   - .rodata.foo.str1.1 at 0x3c050000 (NOT nm-covered — anonymous string pool)
    // After the fix the second range gets a synthetic "foo" row.
    let ranges = vec![
        InputSectionRange {
            addr: 0x42000020,
            size: 0x40,
            output_section: ".flash.text".into(),
            input_section: ".text.foo".into(),
            archive: Some("libFastLED.a".into()),
            object: "fl.foo.cpp.o".into(),
        },
        InputSectionRange {
            addr: 0x3c050000,
            size: 0x10,
            output_section: ".flash.rodata".into(),
            input_section: ".rodata.foo.str1.1".into(),
            archive: Some("libFastLED.a".into()),
            object: "fl.foo.cpp.o".into(),
        },
    ];
    let nm = vec![(0x42000020u64, 0x40u64, 'T', "foo".to_string())];
    let demangled = vec!["foo".to_string()];
    let map = build_fine_grained_map(
        "fw.elf".into(),
        Some("fw.map".into()),
        nm,
        demangled,
        ranges,
    );

    assert_eq!(
        map.symbols.len(),
        2,
        "expected one nm + one map-derived row"
    );
    let nm_row = map.symbols.iter().find(|s| s.source == "nm").unwrap();
    assert_eq!(nm_row.output_section.as_deref(), Some(".flash.text"));
    assert_eq!(nm_row.size, 0x40);
    let synth = map
        .symbols
        .iter()
        .find(|s| s.source == "map-derived")
        .unwrap();
    assert_eq!(synth.mangled, "foo");
    assert_eq!(synth.output_section.as_deref(), Some(".flash.rodata"));
    assert_eq!(synth.size, 0x10);
    assert_eq!(synth.archive.as_deref(), Some("libFastLED.a"));
    assert_eq!(synth.sym_type, 'r');
    assert_eq!(map.total_flash, 0x40 + 0x10);
}

#[test]
fn synthesised_rows_skip_nm_covered_text() {
    // If nm already produces a symbol for an address range, we
    // must not also emit a map-derived row for the same range.
    let ranges = vec![InputSectionRange {
        addr: 0x42000020,
        size: 0x40,
        output_section: ".flash.text".into(),
        input_section: ".text._ZN3foo3barEv".into(),
        archive: Some("libA.a".into()),
        object: "foo.o".into(),
    }];
    let nm = vec![(0x42000020u64, 0x40u64, 'T', "_ZN3foo3barEv".to_string())];
    let demangled = vec!["foo::bar()".to_string()];
    let map = build_fine_grained_map(
        "fw.elf".into(),
        Some("fw.map".into()),
        nm,
        demangled,
        ranges,
    );
    assert_eq!(map.symbols.len(), 1);
    assert_eq!(map.symbols[0].source, "nm");
}

#[test]
fn region_from_output_section_classifies_correctly() {
    assert_eq!(
        region_from_output_section(".flash.text"),
        Some(MemoryRegion::Flash)
    );
    assert_eq!(
        region_from_output_section(".flash.rodata"),
        Some(MemoryRegion::Flash)
    );
    assert_eq!(
        region_from_output_section(".iram0.text"),
        Some(MemoryRegion::Flash)
    );
    assert_eq!(
        region_from_output_section(".dram0.data"),
        Some(MemoryRegion::Ram)
    );
    assert_eq!(
        region_from_output_section(".bss.foo"),
        Some(MemoryRegion::Ram)
    );
    assert!(region_from_output_section(".debug_info").is_none());
}

#[test]
fn nm_range_covers_overlap_detection() {
    let mut nm_covered = BTreeMap::new();
    nm_covered.insert(0x1000u64, 0x10u64);
    nm_covered.insert(0x2000u64, 0x20u64);
    // Exact match
    assert!(nm_range_covers(&nm_covered, 0x1000, 0x10));
    // Overlap at start
    assert!(nm_range_covers(&nm_covered, 0x1008, 0x8));
    // Adjacent — does not overlap
    assert!(!nm_range_covers(&nm_covered, 0x1010, 0x10));
    // Completely past
    assert!(!nm_range_covers(&nm_covered, 0x3000, 0x10));
    // Completely before
    assert!(!nm_range_covers(&nm_covered, 0x500, 0x10));
    // Zero size — never covers
    assert!(!nm_range_covers(&nm_covered, 0x1000, 0));
}

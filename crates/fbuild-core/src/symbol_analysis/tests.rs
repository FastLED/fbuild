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

/// fbuild#417 regression: the exact rodata rows from the FastLED #2473
/// symbol audit that led to a ~95 KB phantom-bloat report. Three of
/// the four heaviest "rodata candidates" the audit picked were
/// `--gc-sections`-tombstoned (address `0x00000000`); only the fourth
/// — `esp_err_to_name` — was actually live. The parser MUST drop the
/// three tombstones so downstream analysis can't sum phantom bytes.
///
/// The snippet below mirrors the map-file shape from the issue
/// verbatim (output section header in the link view + four rodata
/// input rows, three at `0x00000000` and one at a real flash
/// address). See:
///   - https://github.com/FastLED/fbuild/issues/417
///   - https://github.com/FastLED/FastLED/issues/2473#issuecomment-4628287075
#[test]
fn parse_linker_map_drops_fastled_2473_tombstones() {
    let text = "\
Linker script and memory map

.flash.rodata   0x3c000020    0x10000
 .rodata.embedded
                0x00000000    0x110f8 path/libmbedtls.a(x509_crt_bundle.S.obj)
 .rodata.str1.1 0x00000000     0x2b64 path/libmesh.a(mesh_parent.o)
 .rodata.huffTable
                0x00000000     0x2124 path/libFastLED.a(third_party+.cpp.o)
 .rodata.str1.1 0x3c000020     0x1776 path/libesp_common.a(esp_err_to_name.c.obj)
";
    let ranges = parse_linker_map(text);
    // Exactly one live row survives — `esp_err_to_name`. The three
    // tombstones (x509_crt_bundle, mesh_parent, huffTable) are
    // dropped because their address is `0x00000000`.
    assert_eq!(
        ranges.len(),
        1,
        "expected exactly 1 live row, got {}: {:?}",
        ranges.len(),
        ranges.iter().map(|r| &r.input_section).collect::<Vec<_>>()
    );
    let live = &ranges[0];
    assert_eq!(live.archive.as_deref(), Some("libesp_common.a"));
    assert_eq!(live.object, "esp_err_to_name.c.obj");
    assert_eq!(live.addr, 0x3c000020);
    assert_eq!(live.size, 0x1776);
    // Phantom row attribution must NOT appear under any archive name
    // — guard against a future refactor that forgets the addr filter.
    let mbedtls_phantom = ranges
        .iter()
        .any(|r| r.archive.as_deref() == Some("libmbedtls.a"));
    assert!(
        !mbedtls_phantom,
        "fbuild#417: x509_crt_bundle tombstone must not be reported as a live mbedtls.a row"
    );
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

// --- LoadedRegion / retain_loaded_symbols ---

fn sample_symbol(addr: u64, size: u64, region: MemoryRegion, name: &str) -> FineGrainedSymbol {
    FineGrainedSymbol {
        mangled: name.to_string(),
        demangled: name.to_string(),
        address: addr,
        size,
        sym_type: match region {
            MemoryRegion::Flash => 'T',
            MemoryRegion::Ram => 'B',
        },
        region,
        archive: None,
        object: None,
        output_section: None,
        source: "nm".to_string(),
        referenced_by: Vec::new(),
        references_to: Vec::new(),
    }
}

#[test]
fn loaded_region_strict_containment() {
    let r = LoadedRegion {
        start: 0x1000,
        end: 0x2000,
    };
    assert!(r.contains_range(0x1000, 0x100));
    assert!(
        r.contains_range(0x1f00, 0x100),
        "end-aligned must be allowed"
    );
    // Past the end
    assert!(!r.contains_range(0x1f00, 0x101));
    // Below the start
    assert!(!r.contains_range(0x0fff, 0x100));
    // Symbol just past the end (boundary marker case)
    assert!(!r.contains_range(0x2000, 0));
    // Overflow guard
    assert!(!r.contains_range(u64::MAX, 1));
}

/// FastLED/fbuild#XX (the bloat-filter fix): nm enumerates
/// linker-script boundary markers (`__StackTop`, `__flash_arduino_end`,
/// ...) with multi-GB sizes computed as the gap to the next symbol.
/// `retain_loaded_symbols` must drop them so the bloat report shows
/// only bytes actually in the final binary.
#[test]
fn retain_loaded_symbols_drops_boundary_markers() {
    let symbols = vec![
        // Real flash symbol — fully inside the .text PT_LOAD range.
        sample_symbol(0x00026100, 0x40, MemoryRegion::Flash, "real_text"),
        // Real ram symbol — fully inside the RAM PT_LOAD range.
        sample_symbol(0x20006000, 0x80, MemoryRegion::Ram, "real_bss"),
        // __StackTop: address at the very end of RAM, garbage size.
        sample_symbol(0x20040000, 0xdffedfe4, MemoryRegion::Flash, "__StackTop"),
        // __flash_arduino_end: address outside any PT_LOAD region.
        sample_symbol(
            0x000ed000,
            0xfff40fe4,
            MemoryRegion::Flash,
            "__flash_arduino_end",
        ),
        // A symbol that fits in flash but with overflowing size.
        sample_symbol(0x00026100, u64::MAX, MemoryRegion::Flash, "overflow"),
    ];
    let mut map = FineGrainedSymbolMap {
        elf_path: "fixture.elf".into(),
        map_path: None,
        total_flash: 0,
        total_ram: 0,
        symbols,
        sections: Vec::new(),
    };
    // Mirror the nrf52 test fixture's PT_LOAD ranges.
    let regions = vec![
        LoadedRegion {
            start: 0x00026000,
            end: 0x0002dfec,
        },
        LoadedRegion {
            start: 0x20006000,
            end: 0x2003f800,
        },
    ];
    map.retain_loaded_symbols(&regions);
    let kept: Vec<&str> = map.symbols.iter().map(|s| s.demangled.as_str()).collect();
    assert_eq!(kept, vec!["real_text", "real_bss"]);
    assert_eq!(map.total_flash, 0x40);
    assert_eq!(map.total_ram, 0x80);
}

// ---- Issue #459: cref `referenced_by` integration -----------------

#[test]
fn build_fine_grained_map_populates_referenced_by_from_cref() {
    // End-to-end check: an nm symbol whose mangled name appears in
    // the cref table picks up its `referenced_by` list. The defining
    // TU is excluded — it's already on the row's own attribution.
    let ranges = vec![InputSectionRange {
        addr: 0x42000020,
        size: 0x40,
        output_section: ".flash.text".into(),
        input_section: ".text._vfprintf_r".into(),
        archive: Some("libc.a".into()),
        object: "libc_a-vfprintf.o".into(),
    }];
    let nm = vec![(0x42000020u64, 0x40u64, 'T', "_vfprintf_r".to_string())];
    let demangled = vec!["_vfprintf_r".to_string()];
    let mut cref: BTreeMap<String, Vec<SymbolReference>> = BTreeMap::new();
    cref.insert(
        "_vfprintf_r".to_string(),
        vec![
            SymbolReference {
                archive: Some("libc.a".into()),
                object: "libc_a-vprintf.o".into(),
            },
            SymbolReference {
                archive: Some("libc.a".into()),
                object: "libc_a-printf.o".into(),
            },
        ],
    );
    let map = build_fine_grained_map_with_synth(
        "fw.elf".into(),
        Some("fw.map".into()),
        nm,
        demangled,
        ranges,
        &[],
        &cref,
    );
    assert_eq!(map.symbols.len(), 1);
    assert_eq!(
        map.symbols[0].referenced_by,
        vec![
            SymbolReference {
                archive: Some("libc.a".into()),
                object: "libc_a-vprintf.o".into(),
            },
            SymbolReference {
                archive: Some("libc.a".into()),
                object: "libc_a-printf.o".into(),
            },
        ]
    );
}

#[test]
fn build_fine_grained_map_referenced_by_empty_when_no_cref() {
    // The acceptance contract from #459: missing cref data → empty
    // `referenced_by`, never an error. Uses the convenience wrapper
    // `build_fine_grained_map` which passes an empty cref map.
    let ranges = vec![InputSectionRange {
        addr: 0x42000020,
        size: 0x40,
        output_section: ".flash.text".into(),
        input_section: ".text.foo".into(),
        archive: Some("libA.a".into()),
        object: "foo.o".into(),
    }];
    let nm = vec![(0x42000020u64, 0x40u64, 'T', "foo".to_string())];
    let demangled = vec!["foo".to_string()];
    let map = build_fine_grained_map("fw.elf".into(), None, nm, demangled, ranges);
    assert_eq!(map.symbols.len(), 1);
    assert!(
        map.symbols[0].referenced_by.is_empty(),
        "expected [] referenced_by when no cref data, got {:?}",
        map.symbols[0].referenced_by
    );
}

#[test]
fn build_fine_grained_map_populates_referenced_by_on_synth_rows() {
    // Map-derived synthetic rows (anonymous rodata pool case) also
    // pick up cref data when the owning symbol is in the table.
    let ranges = vec![
        InputSectionRange {
            addr: 0x42000020,
            size: 0x40,
            output_section: ".flash.text".into(),
            input_section: ".text.foo".into(),
            archive: Some("libA.a".into()),
            object: "foo.o".into(),
        },
        InputSectionRange {
            addr: 0x3c050000,
            size: 0x10,
            output_section: ".flash.rodata".into(),
            input_section: ".rodata.foo.str1.1".into(),
            archive: Some("libA.a".into()),
            object: "foo.o".into(),
        },
    ];
    let nm = vec![(0x42000020u64, 0x40u64, 'T', "foo".to_string())];
    let demangled = vec!["foo".to_string()];
    let mut cref: BTreeMap<String, Vec<SymbolReference>> = BTreeMap::new();
    cref.insert(
        "foo".to_string(),
        vec![SymbolReference {
            archive: None,
            object: "main.cpp.o".into(),
        }],
    );
    let map = build_fine_grained_map_with_synth(
        "fw.elf".into(),
        Some("fw.map".into()),
        nm,
        demangled,
        ranges,
        &[],
        &cref,
    );
    // Both the nm row and the synthetic rodata row carry the same
    // referenced_by since they share a mangled owner.
    for sym in &map.symbols {
        assert_eq!(sym.referenced_by.len(), 1, "row missing cref: {sym:?}");
        assert_eq!(sym.referenced_by[0].object, "main.cpp.o");
    }
}

#[test]
fn fine_grained_symbol_json_roundtrip_with_legacy_schema() {
    // Pre-#459 JSON omits `referenced_by` entirely; deserialise must
    // tolerate the missing field by defaulting to an empty vec, so
    // older `report.json` files keep loading after the upgrade.
    let legacy_json = r#"{
        "mangled": "foo",
        "demangled": "foo",
        "address": 16384,
        "size": 64,
        "sym_type": "T",
        "region": "flash",
        "archive": "libA.a",
        "object": "foo.o",
        "output_section": ".flash.text",
        "source": "nm"
    }"#;
    let sym: FineGrainedSymbol =
        serde_json::from_str(legacy_json).expect("legacy schema must still parse");
    assert!(sym.referenced_by.is_empty());
}

#[test]
fn retain_loaded_symbols_no_op_when_regions_empty() {
    // Defensive: if the caller couldn't probe PT_LOAD (corrupt ELF,
    // non-ELF input), leave the map untouched rather than empty it.
    let mut map = FineGrainedSymbolMap {
        elf_path: "fixture.elf".into(),
        map_path: None,
        total_flash: 0x40,
        total_ram: 0x80,
        symbols: vec![
            sample_symbol(0x00026100, 0x40, MemoryRegion::Flash, "real_text"),
            sample_symbol(0x20006000, 0x80, MemoryRegion::Ram, "real_bss"),
        ],
        sections: Vec::new(),
    };
    map.retain_loaded_symbols(&[]);
    assert_eq!(map.symbols.len(), 2);
    assert_eq!(map.total_flash, 0x40);
    assert_eq!(map.total_ram, 0x80);
}

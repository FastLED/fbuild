use super::markdown::{
    MarkdownGraphOptions, SidecarOptions, format_markdown_report,
    format_markdown_report_with_graphs, write_sidecar_dot_files,
};
use super::*;

#[test]
fn derive_cppfilt_prefix() {
    let nm = PathBuf::from("/tools/xtensa-esp32s3-elf-nm.exe");
    let cppfilt = derive_cppfilt_path(&nm);
    assert!(
        cppfilt
            .to_string_lossy()
            .ends_with("xtensa-esp32s3-elf-c++filt.exe"),
        "got {}",
        cppfilt.display()
    );
}

#[test]
fn derive_cppfilt_no_prefix() {
    let nm = PathBuf::from("/usr/bin/nm");
    let cppfilt = derive_cppfilt_path(&nm);
    assert_eq!(cppfilt, PathBuf::from("/usr/bin/c++filt"));
}

// ---- build_info / project discovery ----

#[test]
fn elf_from_build_info_pio_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let bi = tmp.path().join("build_info.json");
    std::fs::write(
        &bi,
        r#"{ "esp32s3": { "prog_path": "C:/path/firmware.elf" } }"#,
    )
    .unwrap();
    let elf = elf_from_build_info(&bi).unwrap();
    assert_eq!(elf, PathBuf::from("C:/path/firmware.elf"));
}

#[test]
fn elf_from_build_info_flat_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let bi = tmp.path().join("build_info.json");
    std::fs::write(&bi, r#"{ "prog_path": "/x/y/firmware.elf" }"#).unwrap();
    let elf = elf_from_build_info(&bi).unwrap();
    assert_eq!(elf, PathBuf::from("/x/y/firmware.elf"));
}

#[test]
fn elf_from_build_info_missing_field() {
    let tmp = tempfile::tempdir().unwrap();
    let bi = tmp.path().join("build_info.json");
    std::fs::write(&bi, r#"{ "esp32s3": { "cc_path": "/x" } }"#).unwrap();
    assert!(elf_from_build_info(&bi).is_none());
}

#[test]
fn discover_elf_picks_build_info_first() {
    let tmp = tempfile::tempdir().unwrap();
    let elf_path = tmp.path().join("real_target").join("firmware.elf");
    std::fs::create_dir_all(elf_path.parent().unwrap()).unwrap();
    std::fs::write(&elf_path, b"").unwrap();
    let bi = tmp.path().join("build_info.json");
    std::fs::write(
        &bi,
        format!(
            r#"{{ "x": {{ "prog_path": "{}" }} }}"#,
            // FastLED/fbuild#911 — path-shape slash normalization goes
            // through `NormalizedPath::display_slash()`.
            fbuild_core::path::NormalizedPath::from(elf_path.as_path()).display_slash()
        ),
    )
    .unwrap();
    // Also create a competing .elf elsewhere; build_info should win.
    let competing = tmp.path().join("decoy.elf");
    std::fs::write(&competing, b"").unwrap();
    let found = discover_elf_in_project(tmp.path()).unwrap();
    assert_eq!(found.canonicalize().ok(), elf_path.canonicalize().ok());
}

#[test]
fn discover_elf_falls_back_to_loose_elf() {
    let tmp = tempfile::tempdir().unwrap();
    let elf_path = tmp.path().join("firmware.elf");
    std::fs::write(&elf_path, b"").unwrap();
    let found = discover_elf_in_project(tmp.path()).unwrap();
    assert_eq!(found.canonicalize().ok(), elf_path.canonicalize().ok());
}

#[test]
fn discover_elf_returns_none_when_nothing_found() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(discover_elf_in_project(tmp.path()).is_none());
}

// ---- markdown formatter ----

#[test]
fn format_markdown_report_emits_tables() {
    use fbuild_core::symbol_analysis::{FineGrainedSymbol, FineGrainedSymbolMap, SectionBytes};
    let map = FineGrainedSymbolMap {
        elf_path: "fw.elf".into(),
        map_path: Some("fw.map".into()),
        total_flash: 100,
        total_ram: 50,
        symbols: vec![
            FineGrainedSymbol {
                mangled: "_Z3fooi".into(),
                demangled: "foo(int)".into(),
                address: 0x1000,
                size: 100,
                sym_type: 'T',
                region: fbuild_core::MemoryRegion::Flash,
                archive: Some("libA.a".into()),
                object: Some("foo.o".into()),
                output_section: Some(".flash.text".into()),
                source: "nm".into(),
                referenced_by: Vec::new(),
                references_to: Vec::new(),
                called_by: Vec::new(),
            },
            FineGrainedSymbol {
                mangled: "_Z3barv".into(),
                demangled: "bar()".into(),
                address: 0x2000,
                size: 50,
                sym_type: 'B',
                region: fbuild_core::MemoryRegion::Ram,
                archive: Some("libB.a".into()),
                object: Some("bar.o".into()),
                output_section: Some(".dram0.bss".into()),
                source: "nm".into(),
                referenced_by: Vec::new(),
                references_to: Vec::new(),
                called_by: Vec::new(),
            },
        ],
        sections: Vec::<SectionBytes>::new(),
    };
    let md = format_markdown_report(&map, 5);
    assert!(md.contains("# Symbol analysis: `fw.elf`"));
    assert!(md.contains("**Map file**: `fw.map`"));
    assert!(md.contains("**Flash**: 100 B"));
    assert!(md.contains("**RAM**: 50 B"));
    assert!(md.contains("## Top 1 flash symbols"));
    assert!(md.contains("| 100 | libA.a | foo.o | .flash.text | nm | - | `foo(int)` |"));
    assert!(md.contains("## Top 1 ram symbols"));
    assert!(md.contains("| 50 | libB.a | bar.o | .dram0.bss | nm | - | `bar()` |"));
    assert!(md.contains("## Flash bytes by archive"));
    assert!(md.contains("| 100 | libA.a |"));
}

#[test]
fn format_markdown_report_escapes_pipes_in_symbol_names() {
    use fbuild_core::symbol_analysis::{FineGrainedSymbol, FineGrainedSymbolMap, SectionBytes};
    // operator|| is a real C++ name shape that demangles with pipes.
    let map = FineGrainedSymbolMap {
        elf_path: "fw.elf".into(),
        map_path: None,
        total_flash: 10,
        total_ram: 0,
        symbols: vec![FineGrainedSymbol {
            mangled: "_ZorRKiS_".into(),
            demangled: "operator|(int const&, int const&)".into(),
            address: 0x1000,
            size: 10,
            sym_type: 'T',
            region: fbuild_core::MemoryRegion::Flash,
            archive: None,
            object: None,
            output_section: None,
            source: "nm".into(),
            referenced_by: Vec::new(),
            references_to: Vec::new(),
            called_by: Vec::new(),
        }],
        sections: Vec::<SectionBytes>::new(),
    };
    let md = format_markdown_report(&map, 5);
    assert!(md.contains("operator\\|(int const&, int const&)"));
}

#[test]
fn format_markdown_report_renders_referenced_by_column() {
    // The motivating #459 case: a libc symbol like `_vfprintf_r`
    // shows its non-libc referencers so the agent can answer
    // "who pulled this in?" without spawning a separate query.
    use fbuild_core::symbol_analysis::{
        FineGrainedSymbol, FineGrainedSymbolMap, SectionBytes, SymbolReference,
    };
    let map = FineGrainedSymbolMap {
        elf_path: "fw.elf".into(),
        map_path: None,
        total_flash: 11309,
        total_ram: 0,
        symbols: vec![FineGrainedSymbol {
            mangled: "_vfprintf_r".into(),
            demangled: "_vfprintf_r".into(),
            address: 0x4000,
            size: 11309,
            sym_type: 'T',
            region: fbuild_core::MemoryRegion::Flash,
            archive: Some("libc.a".into()),
            object: Some("libc_a-vfprintf.o".into()),
            output_section: Some(".flash.text".into()),
            source: "nm".into(),
            referenced_by: vec![
                SymbolReference {
                    archive: Some("libc.a".into()),
                    object: "libc_a-vprintf.o".into(),
                },
                SymbolReference {
                    archive: Some("libc.a".into()),
                    object: "libc_a-printf.o".into(),
                },
                SymbolReference {
                    archive: Some("libc.a".into()),
                    object: "libc_a-fprintf.o".into(),
                },
                SymbolReference {
                    archive: Some("liblog.a".into()),
                    object: "log_write.c.obj".into(),
                },
                SymbolReference {
                    archive: Some("libmbedcrypto.a".into()),
                    object: "sha512.c.obj".into(),
                },
            ],
            references_to: Vec::new(),
            called_by: Vec::new(),
        }],
        sections: Vec::<SectionBytes>::new(),
    };
    let md = format_markdown_report(&map, 5);
    // Header includes the new column.
    assert!(
        md.contains("| Bytes | Archive | Object | Section | Source | Referenced by | Symbol |")
    );
    // Cell shows top-3 referencers + "(… and 2 more)" overflow.
    assert!(
        md.contains("libc.a(libc_a-vprintf.o), libc.a(libc_a-printf.o), libc.a(libc_a-fprintf.o), (… and 2 more)"),
        "expected top-3 + overflow in referenced_by cell, got:\n{md}"
    );
}

/// fbuild #463: when graph embedding is enabled, the top-N
/// symbols carry an inline `dot` fenced block under a
/// `<details>` summary. AI-friendliness is the design goal —
/// a fresh agent should be able to answer "what pulls in X?"
/// from `report.md` alone.
#[test]
fn markdown_report_with_graphs_embeds_dot_blocks_for_top_symbols() {
    use fbuild_core::symbol_analysis::{
        FineGrainedSymbol, FineGrainedSymbolMap, GraphConfig, SectionBytes, SymbolReference,
    };
    let map = FineGrainedSymbolMap {
        elf_path: "fw.elf".into(),
        map_path: None,
        total_flash: 11_309,
        total_ram: 0,
        symbols: vec![FineGrainedSymbol {
            mangled: "_vfprintf_r".into(),
            demangled: "_vfprintf_r".into(),
            address: 0x4000,
            size: 11_309,
            sym_type: 'T',
            region: fbuild_core::MemoryRegion::Flash,
            archive: Some("libc.a".into()),
            object: Some("libc_a-vfprintf.o".into()),
            output_section: Some(".flash.text".into()),
            source: "nm".into(),
            referenced_by: vec![SymbolReference {
                archive: Some("liblog.a".into()),
                object: "log_write.c.obj".into(),
            }],
            references_to: Vec::new(),
            called_by: Vec::new(),
        }],
        sections: Vec::<SectionBytes>::new(),
    };
    let md = format_markdown_report_with_graphs(
        &map,
        5,
        &MarkdownGraphOptions {
            enabled: true,
            graph_top: 10,
            config: GraphConfig::default(),
        },
    );
    // #471 renamed this from "back-reference graphs" to
    // "symbol graphs" because the section now shows bidirectional
    // graphs (callers ← root → callees), not just backref.
    assert!(
        md.contains("## Top 1 symbol graphs"),
        "missing symbol-graph section header in:\n{md}"
    );
    assert!(
        md.contains("<details>")
            && md.contains("<summary>Bidirectional graph (callers ← root → callees,"),
        "missing details summary in:\n{md}"
    );
    assert!(
        md.contains("```dot") && md.contains("digraph backref"),
        "missing fenced dot block in:\n{md}"
    );
    // Closure tag is present so the embedded block doesn't bleed
    // into the next section.
    assert!(md.contains("</details>"));
}

/// #471: the per-symbol section MUST embed a "Top callees"
/// dual-ranking sub-table when `references_to` is populated.
/// The sub-table shows the heaviest and the most-shared callees
/// side-by-side, so an AI optimisation pass can tell whether
/// to chase a fat callee or a popular hub.
#[test]
fn markdown_report_emits_dual_ranked_callees_subtable() {
    use fbuild_core::symbol_analysis::{
        FineGrainedSymbol, FineGrainedSymbolMap, GraphConfig, SectionBytes, SymbolReference,
    };
    let root = FineGrainedSymbol {
        mangled: "_Z13ClocklessIdf5v".into(),
        demangled: "ClocklessIdf5".into(),
        address: 0x1000,
        size: 10_000,
        sym_type: 'T',
        region: fbuild_core::MemoryRegion::Flash,
        archive: Some("libFastLED.a".into()),
        object: Some("clockless_idf5.cpp.o".into()),
        output_section: Some(".flash.text".into()),
        source: "nm".into(),
        referenced_by: Vec::new(),
        references_to: vec![
            "esp_log_write".into(),
            "rmt_tx_start".into(),
            "fl_lerp8".into(),
            "small_helper_4".into(),
            "small_helper_5".into(),
        ],
        called_by: Vec::new(),
    };
    let callees = vec![
        FineGrainedSymbol {
            mangled: "esp_log_write".into(),
            demangled: "esp_log_write".into(),
            address: 0x2000,
            size: 2_000,
            sym_type: 'T',
            region: fbuild_core::MemoryRegion::Flash,
            archive: Some("libesp.a".into()),
            object: Some("log.c.o".into()),
            output_section: Some(".flash.text".into()),
            source: "nm".into(),
            referenced_by: (0..10)
                .map(|i| SymbolReference {
                    archive: None,
                    object: format!("caller_{i}.o"),
                })
                .collect(),
            references_to: Vec::new(),
            called_by: Vec::new(),
        },
        FineGrainedSymbol {
            mangled: "rmt_tx_start".into(),
            demangled: "rmt_tx_start".into(),
            address: 0x3000,
            size: 800,
            sym_type: 'T',
            region: fbuild_core::MemoryRegion::Flash,
            archive: Some("libesp.a".into()),
            object: Some("rmt.c.o".into()),
            output_section: Some(".flash.text".into()),
            source: "nm".into(),
            referenced_by: vec![SymbolReference {
                archive: None,
                object: "main.cpp.o".into(),
            }],
            references_to: Vec::new(),
            called_by: Vec::new(),
        },
        FineGrainedSymbol {
            mangled: "fl_lerp8".into(),
            demangled: "fl_lerp8".into(),
            address: 0x4000,
            size: 60,
            sym_type: 'T',
            region: fbuild_core::MemoryRegion::Flash,
            archive: Some("libFastLED.a".into()),
            object: Some("math.cpp.o".into()),
            output_section: Some(".flash.text".into()),
            source: "nm".into(),
            referenced_by: Vec::new(),
            references_to: Vec::new(),
            called_by: Vec::new(),
        },
    ];
    let mut all = vec![root];
    all.extend(callees);
    let map = FineGrainedSymbolMap {
        elf_path: "test.elf".into(),
        map_path: None,
        total_flash: 12_860,
        total_ram: 0,
        symbols: all,
        sections: Vec::<SectionBytes>::new(),
    };
    let md = format_markdown_report_with_graphs(
        &map,
        5,
        &MarkdownGraphOptions {
            enabled: true,
            graph_top: 10,
            config: GraphConfig::default(),
        },
    );
    assert!(
        md.contains("#### Top callees (dual ranking)"),
        "missing dual-ranking sub-table header in:\n{md}"
    );
    // esp_log_write is the heaviest callee (2000 B) AND the most
    // shared (10 callers). Must appear on both axes.
    assert!(md.contains("esp_log_write"));
    // The "by callee size" column header must mention size; the
    // "shared with" column header must mention sharing.
    assert!(md.contains("by callee size (B)"));
    assert!(md.contains("by callees shared with"));
    // The "other" bucket: 5 callees, top-3 each, intersection at
    // least esp_log_write, so other > 0.
    assert!(
        md.contains("more callees, see graph below"),
        "missing 'other' bucket row in:\n{md}"
    );
}

/// `format_markdown_report` (without `_with_graphs`) MUST NOT
/// embed graphs — protects pre-#463 markdown for callers that
/// haven't opted in.
#[test]
fn markdown_report_legacy_path_skips_graph_blocks() {
    use fbuild_core::symbol_analysis::{FineGrainedSymbol, FineGrainedSymbolMap, SectionBytes};
    let map = FineGrainedSymbolMap {
        elf_path: "fw.elf".into(),
        map_path: None,
        total_flash: 10,
        total_ram: 0,
        symbols: vec![FineGrainedSymbol {
            mangled: "main".into(),
            demangled: "main".into(),
            address: 0x4000,
            size: 10,
            sym_type: 'T',
            region: fbuild_core::MemoryRegion::Flash,
            archive: None,
            object: Some("main.cpp.o".into()),
            output_section: Some(".flash.text".into()),
            source: "nm".into(),
            referenced_by: Vec::new(),
            references_to: Vec::new(),
            called_by: Vec::new(),
        }],
        sections: Vec::<SectionBytes>::new(),
    };
    let md = format_markdown_report(&map, 5);
    assert!(!md.contains("```dot"));
    assert!(!md.contains("back-reference graphs"));
}

#[test]
fn sidecar_dot_files_written_for_symbols_above_min_bytes() {
    use fbuild_core::symbol_analysis::{
        FineGrainedSymbol, FineGrainedSymbolMap, GraphConfig, SectionBytes,
    };
    let tmp = tempfile::tempdir().unwrap();
    let map = FineGrainedSymbolMap {
        elf_path: "fw.elf".into(),
        map_path: None,
        total_flash: 1_200,
        total_ram: 0,
        symbols: vec![
            FineGrainedSymbol {
                mangled: "big".into(),
                demangled: "big".into(),
                address: 0x1000,
                size: 1_000,
                sym_type: 'T',
                region: fbuild_core::MemoryRegion::Flash,
                archive: None,
                object: Some("main.o".into()),
                output_section: Some(".text".into()),
                source: "nm".into(),
                referenced_by: Vec::new(),
                references_to: Vec::new(),
                called_by: Vec::new(),
            },
            FineGrainedSymbol {
                mangled: "tiny".into(),
                demangled: "tiny".into(),
                address: 0x2000,
                size: 100,
                sym_type: 'T',
                region: fbuild_core::MemoryRegion::Flash,
                archive: None,
                object: Some("main.o".into()),
                output_section: Some(".text".into()),
                source: "nm".into(),
                referenced_by: Vec::new(),
                references_to: Vec::new(),
                called_by: Vec::new(),
            },
        ],
        sections: Vec::<SectionBytes>::new(),
    };
    let written = write_sidecar_dot_files(
        &map,
        tmp.path(),
        &SidecarOptions {
            enabled: true,
            min_bytes: 500,
            config: GraphConfig::default(),
        },
    )
    .unwrap();
    assert_eq!(written, 1, "only `big` (1000 B) clears the 500 B threshold");
    // Locate the sidecar — rank 1 (largest), demangled "big".
    let graphs = tmp.path().join("graphs");
    assert!(graphs.is_dir());
    let entries: Vec<_> = std::fs::read_dir(&graphs).unwrap().collect();
    assert_eq!(entries.len(), 1);
    let path = entries[0].as_ref().unwrap().path();
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    assert!(
        name.starts_with("0001_") && name.ends_with(".dot"),
        "got {name}"
    );
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.starts_with("digraph"));
}

#[test]
fn sidecar_disabled_writes_nothing() {
    use fbuild_core::symbol_analysis::{
        FineGrainedSymbol, FineGrainedSymbolMap, GraphConfig, SectionBytes,
    };
    let tmp = tempfile::tempdir().unwrap();
    let map = FineGrainedSymbolMap {
        elf_path: "fw.elf".into(),
        map_path: None,
        total_flash: 1_000,
        total_ram: 0,
        symbols: vec![FineGrainedSymbol {
            mangled: "x".into(),
            demangled: "x".into(),
            address: 0x1000,
            size: 1_000,
            sym_type: 'T',
            region: fbuild_core::MemoryRegion::Flash,
            archive: None,
            object: Some("x.o".into()),
            output_section: Some(".text".into()),
            source: "nm".into(),
            referenced_by: Vec::new(),
            references_to: Vec::new(),
            called_by: Vec::new(),
        }],
        sections: Vec::<SectionBytes>::new(),
    };
    let written = write_sidecar_dot_files(
        &map,
        tmp.path(),
        &SidecarOptions {
            enabled: false,
            min_bytes: 0,
            config: GraphConfig::default(),
        },
    )
    .unwrap();
    assert_eq!(written, 0);
    // graphs/ dir should not have been created either.
    assert!(!tmp.path().join("graphs").exists());
}

#[test]
fn format_markdown_report_referenced_by_empty_renders_dash() {
    use fbuild_core::symbol_analysis::{FineGrainedSymbol, FineGrainedSymbolMap, SectionBytes};
    let map = FineGrainedSymbolMap {
        elf_path: "fw.elf".into(),
        map_path: None,
        total_flash: 10,
        total_ram: 0,
        symbols: vec![FineGrainedSymbol {
            mangled: "main".into(),
            demangled: "main".into(),
            address: 0x4000,
            size: 10,
            sym_type: 'T',
            region: fbuild_core::MemoryRegion::Flash,
            archive: None,
            object: Some("main.cpp.o".into()),
            output_section: Some(".flash.text".into()),
            source: "nm".into(),
            referenced_by: Vec::new(),
            references_to: Vec::new(),
            called_by: Vec::new(),
        }],
        sections: Vec::<SectionBytes>::new(),
    };
    let md = format_markdown_report(&map, 5);
    // The "Referenced by" cell is `-` when no cref data exists.
    assert!(
        md.contains("| 10 | (none) | main.cpp.o | .flash.text | nm | - | `main` |"),
        "expected dash in referenced_by cell, got:\n{md}"
    );
}

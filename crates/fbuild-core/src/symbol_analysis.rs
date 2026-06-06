//! Fine-grained per-symbol bloat analysis.
//!
//! Glues together three sources of truth to produce a single row per live
//! symbol in an ELF — *demangled name*, size, address, sym-type, region,
//! plus *which archive + object file + output section* the linker placed
//! it in. Designed for diffing two builds at the symbol level so growth
//! between releases can be pinpointed (e.g. "+74 KB came from
//! `libFastLED.a(fl.channels+.cpp.o):fl::Channel::showPixels`").
//!
//! Sources:
//! - `nm --print-size --size-sort --reverse-sort -S <elf>` → mangled name,
//!   address, size, sym-type. Pure address+size facts.
//! - `<linker>.map` → per-input-section ranges with the source archive
//!   and object. Carries the attribution information `nm` does not.
//! - `c++filt` → demangling (subprocess; not done in this module — the
//!   caller passes already-demangled names alongside mangled ones).
//!
//! The parsing in this file is intentionally **pure** (no subprocess,
//! no fs I/O for tool invocation) so it can be unit-tested without a
//! cross toolchain. Subprocess drivers live in `fbuild_build`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::MemoryRegion;

/// A single live symbol with full attribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FineGrainedSymbol {
    /// Mangled name as it appears in `nm` output.
    pub mangled: String,
    /// Demangled name (== mangled for C symbols / when c++filt unavailable).
    pub demangled: String,
    /// Symbol load address (decimal).
    pub address: u64,
    /// Size in bytes (from `nm --print-size`).
    pub size: u64,
    /// nm type letter: T t W w R r D d B b ...
    pub sym_type: char,
    /// Flash vs Ram, derived from sym_type.
    pub region: MemoryRegion,
    /// Source archive label (e.g. `"libFastLED.a"`) if attributable.
    pub archive: Option<String>,
    /// Object file member inside the archive (e.g.
    /// `"fl.channels+.cpp.o"`), or the bare object file when no
    /// archive is involved.
    pub object: Option<String>,
    /// Output section the symbol lives in (e.g. `".flash.text"`).
    pub output_section: Option<String>,
}

/// Per `(archive, object, output_section)` byte roll-up taken straight
/// from the linker map. Captures bytes that **don't** appear as named
/// symbols in `nm`, in particular the merged `.flash.rodata` string
/// pool (anonymous `.rodata.<func>.str1.1` sub-sections that hold all
/// the `FL_WARN`/`FL_LOG`/format strings emitted by the TU). Without
/// this view the diff misses substantial rodata bloat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionBytes {
    pub archive: Option<String>,
    pub object: String,
    pub output_section: String,
    pub bytes: u64,
}

/// The complete per-symbol view of a single binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FineGrainedSymbolMap {
    pub elf_path: String,
    pub map_path: Option<String>,
    pub total_flash: u64,
    pub total_ram: u64,
    pub symbols: Vec<FineGrainedSymbol>,
    /// Per-`(archive, object, output_section)` byte totals from the map.
    /// Empty when no map file was supplied. Complements `symbols` by
    /// covering bytes that don't have a named `nm` entry (e.g. merged
    /// rodata string pools).
    #[serde(default)]
    pub sections: Vec<SectionBytes>,
}

/// One address range placed by the linker into an output section.
#[derive(Debug, Clone)]
pub struct InputSectionRange {
    pub addr: u64,
    pub size: u64,
    pub output_section: String,
    /// e.g. `".flash.text"`. Inherited from the enclosing output section
    /// listing in the map file.
    pub input_section: String,
    /// e.g. `".text._ZN2fl7ChannelXXX"` — the per-symbol input section
    /// name. Used as a fallback when `nm`'s symbol address doesn't fall
    /// neatly inside any range (relaxed sections, weak collapses).
    pub archive: Option<String>,
    pub object: String,
}

/// Parse one line of `nm --print-size --size-sort --reverse-sort -S` output.
///
/// Format: `<addr-hex> <size-hex> <type> <name>` where address and size
/// are hexadecimal (no `0x` prefix), type is a single letter, and name
/// may contain spaces for C++ templates. Returns `None` for lines that
/// don't carry a sized symbol (the `nm` "no size" output is filtered
/// out — those rows have only 3 whitespace fields).
pub fn parse_nm_line(line: &str) -> Option<(u64, u64, char, String)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let addr = u64::from_str_radix(parts[0], 16).ok()?;
    let size = u64::from_str_radix(parts[1], 16).ok()?;
    let ch = parts[2].chars().next()?;
    let name = parts[3..].join(" ");
    Some((addr, size, ch, name))
}

/// Parse the body of `nm --print-size --size-sort --reverse-sort -S <elf>`.
pub fn parse_nm_output(output: &str) -> Vec<(u64, u64, char, String)> {
    output
        .lines()
        .filter_map(parse_nm_line)
        .filter(|(_, sz, _, _)| *sz > 0)
        .collect()
}

/// Extract `"libFastLED.a"` from a path like
/// `.pio/build/esp32s3/lib0d9/libFastLED.a(fl.channels+.cpp.o)`. Also
/// supports bare object files like `.pio/build/esp32s3/src/main.cpp.o`,
/// in which case the archive is `None` and the object is the basename.
pub fn extract_archive_and_object(src: &str) -> (Option<String>, String) {
    let trimmed = src.trim();
    // Form A: ".../<archive>.a(<object>)"
    if let Some(open) = trimmed.find(".a(") {
        let archive_end = open + 2; // include ".a"
        let archive_path = &trimmed[..archive_end];
        let archive = archive_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(archive_path)
            .to_string();
        let object_start = open + 3;
        let object_end = trimmed[object_start..]
            .find(')')
            .map(|p| object_start + p)
            .unwrap_or(trimmed.len());
        let object = trimmed[object_start..object_end].to_string();
        return (Some(archive), object);
    }
    // Form B: ".../<file>.o" — no archive, just an object on disk.
    let basename = trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(trimmed)
        .to_string();
    (None, basename)
}

/// Parse a PIO-style GNU `ld --print-map` output and return the per-input-section
/// ranges that contain live bytes (size > 0 AND address != 0).
///
/// The map file has two large views: the per-archive *input* view (mostly
/// tombstones at address 0x00000000 after `--gc-sections` strips dead
/// sections) and the per-output-section *layout* view, which is what we
/// care about. Output sections look like:
///
/// ```text
/// .flash.text     0x42000020    0x4026c
///  .literal._ZN...
///                 0x42000020       0x10 .pio/build/esp32s3/lib0d9/libFastLED.a(fl.foo+.cpp.o)
///  .text.startup.main
///                 0x42000030       0x40 .pio/build/esp32s3/src/main.cpp.o
/// ```
///
/// This parser walks every output section whose header looks like
/// `.<name>  0x<addr>  0x<size>` at column 0 and harvests the input section
/// rows nested under it.
pub fn parse_linker_map(map_text: &str) -> Vec<InputSectionRange> {
    let mut out = Vec::new();
    let mut in_link_view = false;
    let mut current_output: Option<String> = None;
    let mut pending_input_section: Option<String> = None;

    for raw in map_text.lines() {
        if !in_link_view {
            if raw.starts_with("Linker script and memory map") {
                in_link_view = true;
            }
            continue;
        }

        // Output section header: starts at column 0 with a dot, has addr+size.
        if raw.starts_with('.') {
            // Parse "<sect> 0x<addr> 0x<size>" (anything after is ignored).
            let mut it = raw.split_whitespace();
            let sect = it.next().unwrap_or("");
            if let (Some(addr_s), Some(_size_s)) = (it.next(), it.next()) {
                if addr_s.starts_with("0x") {
                    current_output = Some(sect.to_string());
                    pending_input_section = None;
                    continue;
                }
            }
            // Some lines starting with '.' are linker keywords ('.' at col 0
            // with no addr+size). Ignore.
            continue;
        }

        // Lines inside an output section. Two sub-formats:
        //   A:  " .input.section 0x<addr> 0x<size> <source>"
        //   B:  " .input.section"  (name on its own line)
        //       "                0x<addr> 0x<size> <source>"
        let stripped = raw.trim_start();
        if !stripped.starts_with('.') && !stripped.starts_with("0x") {
            // Filler — symbol pinning, fill bytes, etc. Skip.
            pending_input_section = None;
            continue;
        }

        // Try form A: section + addr + size + source.
        if stripped.starts_with('.') {
            let mut it = stripped.split_whitespace();
            let sect = it.next().unwrap_or("").to_string();
            if let (Some(a), Some(s)) = (it.next(), it.next()) {
                if a.starts_with("0x") && s.starts_with("0x") {
                    let src = it.collect::<Vec<_>>().join(" ");
                    let addr = u64::from_str_radix(&a[2..], 16).unwrap_or(0);
                    let size = u64::from_str_radix(&s[2..], 16).unwrap_or(0);
                    pending_input_section = None;
                    if size > 0 && addr != 0 && !src.is_empty() {
                        if let Some(ref out_sect) = current_output {
                            let (archive, object) = extract_archive_and_object(&src);
                            out.push(InputSectionRange {
                                addr,
                                size,
                                output_section: out_sect.clone(),
                                input_section: sect,
                                archive,
                                object,
                            });
                        }
                    }
                    continue;
                }
            }
            // It's just the input section name; stash for the next line.
            pending_input_section = Some(sect);
            continue;
        }

        // Form B continuation: addr + size + source.
        if let Some(name) = pending_input_section.take() {
            let mut it = stripped.split_whitespace();
            if let (Some(a), Some(s)) = (it.next(), it.next()) {
                if a.starts_with("0x") && s.starts_with("0x") {
                    let src = it.collect::<Vec<_>>().join(" ");
                    let addr = u64::from_str_radix(&a[2..], 16).unwrap_or(0);
                    let size = u64::from_str_radix(&s[2..], 16).unwrap_or(0);
                    if size > 0 && addr != 0 && !src.is_empty() {
                        if let Some(ref out_sect) = current_output {
                            let (archive, object) = extract_archive_and_object(&src);
                            out.push(InputSectionRange {
                                addr,
                                size,
                                output_section: out_sect.clone(),
                                input_section: name,
                                archive,
                                object,
                            });
                        }
                    }
                }
            }
        }
    }

    out
}

/// Classify an nm type letter into Flash vs Ram. Matches the existing
/// `SymbolMap::classify` logic so reports compare apples-to-apples.
pub fn classify_region(sym_type: char) -> Option<MemoryRegion> {
    match sym_type {
        'T' | 't' | 'R' | 'r' | 'W' | 'w' => Some(MemoryRegion::Flash),
        'D' | 'd' | 'B' | 'b' => Some(MemoryRegion::Ram),
        _ => None,
    }
}

/// Build an address-indexed lookup over input section ranges so we can
/// attribute every nm symbol to the range containing it in O(log n).
pub struct InputSectionIndex {
    /// Sorted-by-address map: start_addr → range index.
    by_start: BTreeMap<u64, usize>,
    ranges: Vec<InputSectionRange>,
}

impl InputSectionIndex {
    pub fn build(mut ranges: Vec<InputSectionRange>) -> Self {
        ranges.sort_by_key(|r| r.addr);
        let mut by_start = BTreeMap::new();
        for (idx, r) in ranges.iter().enumerate() {
            by_start.insert(r.addr, idx);
        }
        Self { by_start, ranges }
    }

    /// Find the range whose [addr, addr+size) contains the given address.
    /// Picks the largest start_addr ≤ addr, then verifies containment.
    pub fn lookup(&self, addr: u64) -> Option<&InputSectionRange> {
        let (_, &idx) = self.by_start.range(..=addr).next_back()?;
        let r = &self.ranges[idx];
        if addr < r.addr.saturating_add(r.size) {
            Some(r)
        } else {
            None
        }
    }
}

/// Stitch nm output + map file ranges + demangled-names into a final
/// per-symbol view. Takes already-demangled names so this function stays
/// free of subprocess invocations (and easily unit-testable).
/// Aggregate raw map ranges into `(archive, object, output_section)`
/// byte buckets. Filters out ELF-metadata sections (`.debug_*`, `.xt.*`,
/// `.comment`, `.note*`) that live in the ELF but never make it into
/// the firmware image — counting them would dwarf the real bytes and
/// mislead diffs. Stable sort by bytes-descending so consumers can
/// stream the top contributors without re-sorting.
pub fn rollup_sections(ranges: &[InputSectionRange]) -> Vec<SectionBytes> {
    let mut acc: std::collections::HashMap<(Option<String>, String, String), u64> =
        std::collections::HashMap::new();
    for r in ranges {
        if !is_firmware_section(&r.output_section) {
            continue;
        }
        let key = (
            r.archive.clone(),
            r.object.clone(),
            r.output_section.clone(),
        );
        *acc.entry(key).or_insert(0) += r.size;
    }
    let mut out: Vec<SectionBytes> = acc
        .into_iter()
        .map(|((archive, object, output_section), bytes)| SectionBytes {
            archive,
            object,
            output_section,
            bytes,
        })
        .collect();
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    out
}

/// True for output sections whose bytes end up in the firmware image
/// (text, rodata, IRAM, DRAM, RTC). Excludes ELF metadata that lives
/// in the file but is stripped at flash time (debug info, Xtensa
/// scratch sections, `.comment`, etc.).
pub fn is_firmware_section(name: &str) -> bool {
    let n = name;
    if n.starts_with(".debug")
        || n.starts_with(".xt.")
        || n.starts_with(".xtensa")
        || n.starts_with(".comment")
        || n.starts_with(".note")
        || n.starts_with(".group")
        || n.starts_with(".dynsym")
        || n.starts_with(".strtab")
        || n.starts_with(".symtab")
    {
        return false;
    }
    n.starts_with(".flash")
        || n.starts_with(".iram")
        || n.starts_with(".dram")
        || n.starts_with(".rtc")
        || n.starts_with(".rodata")
        || n.starts_with(".text")
        || n.starts_with(".data")
        || n.starts_with(".bss")
}

pub fn build_fine_grained_map(
    elf_path: String,
    map_path: Option<String>,
    nm_rows: Vec<(u64, u64, char, String)>,
    demangled: Vec<String>,
    ranges: Vec<InputSectionRange>,
) -> FineGrainedSymbolMap {
    assert_eq!(
        nm_rows.len(),
        demangled.len(),
        "nm_rows and demangled must be parallel"
    );
    let sections = rollup_sections(&ranges);
    let index = InputSectionIndex::build(ranges);
    let mut symbols = Vec::with_capacity(nm_rows.len());
    let mut total_flash = 0u64;
    let mut total_ram = 0u64;
    for ((addr, size, sym_type, mangled), demangled) in nm_rows.into_iter().zip(demangled) {
        let Some(region) = classify_region(sym_type) else {
            continue;
        };
        match region {
            MemoryRegion::Flash => total_flash += size,
            MemoryRegion::Ram => total_ram += size,
        }
        let attribution = index.lookup(addr);
        symbols.push(FineGrainedSymbol {
            mangled,
            demangled,
            address: addr,
            size,
            sym_type,
            region,
            archive: attribution.and_then(|r| r.archive.clone()),
            object: attribution.map(|r| r.object.clone()),
            output_section: attribution.map(|r| r.output_section.clone()),
        });
    }
    FineGrainedSymbolMap {
        elf_path,
        map_path,
        total_flash,
        total_ram,
        symbols,
        sections,
    }
}

#[cfg(test)]
mod tests {
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
        let (arc, obj) = extract_archive_and_object(
            ".pio/build/esp32s3/lib0d9/libFastLED.a(fl.channels+.cpp.o)",
        );
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
        assert_eq!(map.total_flash, 0x40);
        assert_eq!(map.total_ram, 0);
    }
}

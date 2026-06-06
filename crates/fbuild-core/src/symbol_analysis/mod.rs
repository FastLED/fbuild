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
    /// Mangled name as it appears in `nm` output (or, for map-derived
    /// rows, the mangled owner extracted from the input section name).
    pub mangled: String,
    /// Demangled name (== mangled for C symbols / when c++filt unavailable).
    pub demangled: String,
    /// Symbol load address (decimal).
    pub address: u64,
    /// Size in bytes. For `nm` rows this is the value from
    /// `--print-size`; for map-derived rows this is the size of the
    /// input section attributed to the owner.
    pub size: u64,
    /// nm type letter: T t W w R r D d B b ... For map-derived
    /// rodata rows this is set to `'r'` (read-only data, weak); for
    /// map-derived literal rows it's `'r'` as well; for `.text` and
    /// `.data` it follows the natural nm convention.
    pub sym_type: char,
    /// Flash vs Ram, derived from sym_type or the output section.
    pub region: MemoryRegion,
    /// Source archive label (e.g. `"libFastLED.a"`) if attributable.
    pub archive: Option<String>,
    /// Object file member inside the archive (e.g.
    /// `"fl.channels+.cpp.o"`), or the bare object file when no
    /// archive is involved.
    pub object: Option<String>,
    /// Output section the symbol lives in (e.g. `".flash.text"`).
    pub output_section: Option<String>,
    /// Provenance: `"nm"` for rows produced by parsing nm output,
    /// `"map-derived"` for rows synthesised from linker map input-
    /// section names that carry the owning symbol but aren't enumerated
    /// by nm (e.g. anonymous merged rodata string pools). Default
    /// `"nm"` is used by `Deserialize` on JSON written by earlier
    /// versions of `fbuild symbols`.
    #[serde(default = "default_source_nm")]
    pub source: String,
}

fn default_source_nm() -> String {
    "nm".to_string()
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

/// Extract the mangled owner symbol from a per-symbol input-section
/// name. Returns `None` when the section doesn't carry an owner
/// (typically: catch-all `.rodata` blocks, compiler-generated runtime
/// helper sections without `-fdata-sections` granularity).
///
/// Recognised forms:
/// - `.text.<owner>` / `.literal.<owner>` — code + Xtensa literal pool
/// - `.rodata.<owner>` / `.rodata.<owner>.str1.<N>` / `.rodata.<owner>.cst<N>`
/// - `.data.<owner>` / `.bss.<owner>` / `.data.rel.ro.<owner>`
/// - `.gnu.linkonce.t.<owner>` / `.gnu.linkonce.r.<owner>` /
///   `.gnu.linkonce.d.<owner>` (COMDAT)
///
/// `<owner>` is whatever non-empty token follows the recognised
/// prefix once any sub-section suffix (`.str1.<N>`, `.cst<N>`) is
/// trimmed. Compiler-emitted owners virtually always begin with `_Z`
/// (C++ mangled) or are plain C identifiers — both are returned
/// verbatim so the caller can demangle.
pub fn extract_owner_from_section(input_section: &str) -> Option<String> {
    // Try each prefix in turn; the first match wins. `.data.rel.ro.`
    // and `.gnu.linkonce.*.` must come before their shorter prefixes
    // so the longer match takes precedence.
    const PREFIXES: &[&str] = &[
        ".data.rel.ro.",
        ".gnu.linkonce.t.",
        ".gnu.linkonce.r.",
        ".gnu.linkonce.d.",
        ".gnu.linkonce.b.",
        ".text.",
        ".literal.",
        ".rodata.",
        ".data.",
        ".bss.",
    ];

    let mut owner: Option<&str> = None;
    for prefix in PREFIXES {
        if let Some(rest) = input_section.strip_prefix(prefix) {
            owner = Some(rest);
            break;
        }
    }
    let owner = owner?;
    if owner.is_empty() {
        return None;
    }

    // Strip trailing sub-section suffixes that appear on string and
    // constant pools attached to the owner. Examples:
    //   _ZN...getNameEv.str1.1   -> _ZN...getNameEv
    //   foo.cst4                  -> foo
    //   foo.cst16                 -> foo
    // We strip only the *last* suffix we recognise; the owner itself
    // may contain dots (rare but legal in templated names emitted by
    // some toolchains), and over-stripping would corrupt it.
    let trimmed = trim_pool_suffix(owner);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Trim a single `.str1.<N>` or `.cst<N>` suffix if present.
fn trim_pool_suffix(s: &str) -> &str {
    // .str1.<digits> at the end
    if let Some(idx) = s.rfind(".str1.") {
        let tail = &s[idx + ".str1.".len()..];
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
            return &s[..idx];
        }
    }
    // .cst<digits> at the end
    if let Some(idx) = s.rfind(".cst") {
        let tail = &s[idx + ".cst".len()..];
        if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
            return &s[..idx];
        }
    }
    s
}

/// Classify the firmware region (Flash vs Ram) from an output section
/// name. Used for map-derived symbols where `nm` type letters aren't
/// available.
pub fn region_from_output_section(name: &str) -> Option<MemoryRegion> {
    if name.starts_with(".flash") || name.starts_with(".rodata") || name.starts_with(".text") {
        Some(MemoryRegion::Flash)
    } else if name.starts_with(".iram") {
        // IRAM is internal SRAM that the firmware image populates at
        // boot — counts as Flash from a "in the binary image" POV.
        Some(MemoryRegion::Flash)
    } else if name.starts_with(".dram") || name.starts_with(".data") || name.starts_with(".bss") {
        Some(MemoryRegion::Ram)
    } else {
        None
    }
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
    build_fine_grained_map_with_synth(elf_path, map_path, nm_rows, demangled, ranges, &[])
}

/// Build a per-symbol view, optionally consuming pre-demangled names
/// for map-derived synthetic symbols.
///
/// `synth_demangled` is a parallel slice to `synth_owners()` (the
/// caller-collected owners, in iteration order matching what
/// [`collect_map_derived_owners`] returns). When empty, synthetic
/// rows carry their mangled name in the `demangled` field; callers
/// who want demangled C++ names should pass the c++filt output.
pub fn build_fine_grained_map_with_synth(
    elf_path: String,
    map_path: Option<String>,
    nm_rows: Vec<(u64, u64, char, String)>,
    demangled: Vec<String>,
    ranges: Vec<InputSectionRange>,
    synth_demangled: &[String],
) -> FineGrainedSymbolMap {
    assert_eq!(
        nm_rows.len(),
        demangled.len(),
        "nm_rows and demangled must be parallel"
    );
    let sections = rollup_sections(&ranges);

    // Track addresses that nm covers so map-derived rows don't double-
    // count text bytes that nm already enumerated. Map ranges that
    // overlap an nm-covered range are skipped from synthesis.
    let mut nm_covered: BTreeMap<u64, u64> = BTreeMap::new(); // addr -> size
    for (addr, size, _, _) in nm_rows.iter() {
        nm_covered.insert(*addr, *size);
    }

    let index = InputSectionIndex::build(ranges.clone());
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
            source: "nm".to_string(),
        });
    }

    // Synthesise rows from map ranges whose input-section name carries
    // an extractable owner but where nm produced no symbol covering
    // those bytes (the merged rodata string pool case).
    let owners = collect_map_derived_owners(&ranges, &nm_covered);
    let resolved_synth: Vec<String> = if synth_demangled.len() == owners.len() {
        synth_demangled.to_vec()
    } else {
        owners
            .iter()
            .map(|(_, mangled, _)| mangled.clone())
            .collect()
    };
    for ((range_idx, mangled, owner_size), demangled) in owners.iter().zip(resolved_synth.iter()) {
        let r = &ranges[*range_idx];
        let Some(region) = region_from_output_section(&r.output_section) else {
            continue;
        };
        match region {
            MemoryRegion::Flash => total_flash += *owner_size,
            MemoryRegion::Ram => total_ram += *owner_size,
        }
        // Pick a sym_type letter that matches the output section's
        // nature so existing aggregators that classify by letter still
        // make sense. Text → 'W' (weak text, since the owner has its
        // canonical 'T' from nm if anywhere). Rodata/literal → 'r'.
        // Data → 'd'. BSS → 'b'.
        let sym_type = sym_type_for_synth(&r.output_section);
        symbols.push(FineGrainedSymbol {
            mangled: mangled.clone(),
            demangled: demangled.clone(),
            address: r.addr,
            size: *owner_size,
            sym_type,
            region,
            archive: r.archive.clone(),
            object: Some(r.object.clone()),
            output_section: Some(r.output_section.clone()),
            source: "map-derived".to_string(),
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

/// Walk map ranges and return `(range_index, mangled_owner, size)` for
/// every range that:
/// 1. lives in a firmware-image section,
/// 2. has an extractable owner from its input section name, and
/// 3. is not covered by any nm-listed symbol's address range.
///
/// Sizes are accumulated per `(range_idx, owner)` — when several
/// input sections share an owner (e.g. `.rodata.foo` and
/// `.rodata.foo.str1.1`) the caller sees them as separate rows; this
/// is the right granularity because each landed at its own address.
pub fn collect_map_derived_owners(
    ranges: &[InputSectionRange],
    nm_covered: &BTreeMap<u64, u64>,
) -> Vec<(usize, String, u64)> {
    let mut out = Vec::new();
    for (idx, r) in ranges.iter().enumerate() {
        if !is_firmware_section(&r.output_section) {
            continue;
        }
        if nm_range_covers(nm_covered, r.addr, r.size) {
            continue;
        }
        let Some(owner) = extract_owner_from_section(&r.input_section) else {
            continue;
        };
        out.push((idx, owner, r.size));
    }
    out
}

/// Return true when any nm-listed symbol's address range overlaps
/// `[addr, addr + size)`.
fn nm_range_covers(nm_covered: &BTreeMap<u64, u64>, addr: u64, size: u64) -> bool {
    if size == 0 {
        return false;
    }
    let end = addr.saturating_add(size);
    // Check the largest nm symbol whose start ≤ end-1, then walk back
    // far enough to catch any whose start+size overlaps.
    let mut cur = nm_covered.range(..end).next_back();
    while let Some((&start, &nsize)) = cur {
        let nend = start.saturating_add(nsize);
        if nend <= addr {
            // This symbol ends before our range; earlier symbols only
            // end even sooner. Done.
            return false;
        }
        if start < end && nend > addr {
            return true;
        }
        cur = nm_covered.range(..start).next_back();
    }
    false
}

/// Map an output section to the sym_type letter we attach to a
/// synthetic row so downstream classifiers that key off the letter
/// still bucket it correctly.
fn sym_type_for_synth(output_section: &str) -> char {
    if output_section.starts_with(".dram") || output_section.starts_with(".data") {
        'd'
    } else if output_section.starts_with(".bss") {
        'b'
    } else if output_section.starts_with(".iram") || output_section.starts_with(".text") {
        // Text without an nm anchor is unusual but possible for
        // linkonce/COMDAT collapses. Mark weak so it doesn't pretend
        // to be a canonical strong symbol.
        'W'
    } else {
        // .flash.rodata, .rodata, .literal — all rodata.
        'r'
    }
}

#[cfg(test)]
mod tests;

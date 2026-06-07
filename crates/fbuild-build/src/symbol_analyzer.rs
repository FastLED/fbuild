//! Driver around `fbuild_core::symbol_analysis` — invokes the cross
//! toolchain (`nm`, `c++filt`) and the linker map alongside an ELF and
//! emits a fully-attributed `FineGrainedSymbolMap`.
//!
//! Designed to work on **any** ELF, including binaries produced by an
//! out-of-band builder (PlatformIO, esp-idf, manual). Drives the same
//! analysis the `fbuild build --symbol-analysis` flag invokes
//! post-link, but is also exposed via the standalone
//! `fbuild symbols <elf>` subcommand.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use fbuild_core::subprocess::run_command_with_stdin;
use fbuild_core::symbol_analysis::graph::{rank_callees_dual, Direction};
use fbuild_core::symbol_analysis::{
    build_fine_grained_map_with_synth, collect_map_derived_owners, parse_cref_table,
    parse_linker_map, parse_nm_output, sanitize_filename, BackrefGraph, FineGrainedSymbolMap,
    GraphConfig, LoadedRegion, SymbolReference, TuIndex,
};
use fbuild_core::{FbuildError, Result};

/// Read the `PT_LOAD` program-header ranges from an ELF. These are the
/// regions that actually get programmed into the device's flash/RAM
/// image at boot — every other byte in the ELF (debug info,
/// `.ARM.attributes`, `.comment`, symbol tables) lives only in the
/// host-side file and never reaches the binary.
///
/// Used by [`analyze_elf`] to drop linker-script boundary symbols
/// (`__StackTop`, `__flash_arduino_end`, ...) that nm reports with
/// nonsense sizes computed from the gap to the next symbol — these
/// inflate the bloat report by gigabytes if not filtered.
pub fn read_pt_load_regions(elf_path: &Path) -> Result<Vec<LoadedRegion>> {
    use object::read::elf::{ElfFile32, ElfFile64, FileHeader, ProgramHeader};
    use object::{Endianness, FileKind};

    let bytes = std::fs::read(elf_path).map_err(|e| {
        FbuildError::BuildFailed(format!(
            "could not read ELF at {} for PT_LOAD probe: {e}",
            elf_path.display()
        ))
    })?;
    let kind = FileKind::parse(&bytes[..]).map_err(|e| {
        FbuildError::BuildFailed(format!(
            "could not identify file kind for {}: {e}",
            elf_path.display()
        ))
    })?;

    let mut regions = Vec::new();
    match kind {
        FileKind::Elf32 => {
            let elf = ElfFile32::<Endianness>::parse(&bytes[..])
                .map_err(|e| FbuildError::BuildFailed(format!("ELF32 parse failed: {e}")))?;
            let endian = elf
                .elf_header()
                .endian()
                .map_err(|e| FbuildError::BuildFailed(format!("ELF32 endian probe failed: {e}")))?;
            for ph in elf.elf_program_headers() {
                if ph.p_type(endian) != object::elf::PT_LOAD {
                    continue;
                }
                let start = u64::from(ph.p_vaddr(endian));
                let size = u64::from(ph.p_memsz(endian));
                if size == 0 {
                    continue;
                }
                regions.push(LoadedRegion {
                    start,
                    end: start.saturating_add(size),
                });
            }
        }
        FileKind::Elf64 => {
            let elf = ElfFile64::<Endianness>::parse(&bytes[..])
                .map_err(|e| FbuildError::BuildFailed(format!("ELF64 parse failed: {e}")))?;
            let endian = elf
                .elf_header()
                .endian()
                .map_err(|e| FbuildError::BuildFailed(format!("ELF64 endian probe failed: {e}")))?;
            for ph in elf.elf_program_headers() {
                if ph.p_type(endian) != object::elf::PT_LOAD {
                    continue;
                }
                let start = ph.p_vaddr(endian);
                let size = ph.p_memsz(endian);
                if size == 0 {
                    continue;
                }
                regions.push(LoadedRegion {
                    start,
                    end: start.saturating_add(size),
                });
            }
        }
        other => {
            return Err(FbuildError::BuildFailed(format!(
                "expected ELF, got {other:?} at {}",
                elf_path.display()
            )));
        }
    }
    Ok(regions)
}

/// Auto-detect the cross-toolchain prefix from the directory containing
/// an `nm` binary. e.g. `xtensa-esp32s3-elf-nm` → prefix
/// `xtensa-esp32s3-elf-`, so `xtensa-esp32s3-elf-c++filt` can be derived.
pub fn derive_cppfilt_path(nm_path: &Path) -> PathBuf {
    let parent = nm_path.parent().unwrap_or(Path::new("."));
    let stem = nm_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = nm_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let cppfilt_stem = if let Some(prefix) = stem.strip_suffix("nm") {
        format!("{prefix}c++filt")
    } else {
        "c++filt".to_string()
    };
    if ext.is_empty() {
        parent.join(cppfilt_stem)
    } else {
        parent.join(format!("{cppfilt_stem}.{ext}"))
    }
}

/// Find the map file that matches an ELF. PlatformIO writes
/// `firmware.map` next to `firmware.elf`. fbuild's native linker writes
/// `<elf-stem>.map` alongside the ELF.
pub fn default_map_path(elf_path: &Path) -> Option<PathBuf> {
    let candidate = elf_path.with_extension("map");
    if candidate.exists() {
        return Some(candidate);
    }
    let parent = elf_path.parent()?;
    let firmware_map = parent.join("firmware.map");
    if firmware_map.exists() {
        return Some(firmware_map);
    }
    None
}

/// Demangle a list of mangled symbol names via `c++filt`.
///
/// Routes through `fbuild_core::subprocess::run_command_with_stdin`,
/// which uses `running_process::NativeProcess` under the hood. The
/// reader thread spawned by `NativeProcess::start()` drains stdout in
/// background while we feed stdin, avoiding the Windows pipe-buffer
/// deadlock that hits when ~3k mangled symbols saturate the 4-8 KB
/// stdout pipe before we finish writing stdin.
///
/// When c++filt can't decode a name it echoes it back unchanged, which
/// is the desired fallback. Output stays parallel to the input.
pub fn demangle_batch(mangled: &[String], cppfilt_path: &Path) -> Result<Vec<String>> {
    if mangled.is_empty() {
        return Ok(Vec::new());
    }
    let stdin_data = mangled.join("\n");
    let cppfilt_s = cppfilt_path.to_string_lossy().to_string();
    let args = [cppfilt_s.as_str()];
    let result =
        run_command_with_stdin(&args, stdin_data.as_bytes(), None, None, None).map_err(|e| {
            FbuildError::BuildFailed(format!(
                "failed to run c++filt at {}: {e}",
                cppfilt_path.display()
            ))
        })?;
    if !result.success() {
        return Err(FbuildError::BuildFailed(format!(
            "c++filt failed (exit={}): {}",
            result.exit_code, result.stderr
        )));
    }
    let mut demangled: Vec<String> = result.stdout.lines().map(|s| s.to_string()).collect();
    // Pad with the mangled name in case c++filt dropped trailing blanks.
    while demangled.len() < mangled.len() {
        demangled.push(mangled[demangled.len()].clone());
    }
    demangled.truncate(mangled.len());
    Ok(demangled)
}

/// Configuration for `analyze_elf`.
pub struct AnalyzeConfig<'a> {
    pub elf_path: &'a Path,
    pub map_path: Option<&'a Path>,
    pub nm_path: &'a Path,
    pub cppfilt_path: Option<&'a Path>,
    /// Optional objdump used to populate per-symbol forward refs
    /// (`references_to`). When absent, the analyzer skips the
    /// forward-call extraction and leaves `references_to` empty —
    /// existing backref-only consumers don't see a behaviour change.
    /// Wire from `build_info.json::objdump_path` (#428).
    pub objdump_path: Option<&'a Path>,
}

/// Run nm + c++filt + map-file parse and return the fully-attributed
/// per-symbol map.
pub fn analyze_elf(cfg: AnalyzeConfig<'_>) -> Result<FineGrainedSymbolMap> {
    use fbuild_core::subprocess::run_command;

    let nm_path_s = cfg.nm_path.to_string_lossy().to_string();
    let elf_s = cfg.elf_path.to_string_lossy().to_string();
    let args = [
        nm_path_s.as_str(),
        "--print-size",
        "--size-sort",
        "--reverse-sort",
        "-S",
        elf_s.as_str(),
    ];
    let result = run_command(&args, None, None, None)?;
    if !result.success() {
        return Err(FbuildError::BuildFailed(format!(
            "nm failed: {}",
            result.stderr
        )));
    }

    let nm_rows = parse_nm_output(&result.stdout);
    let mangled: Vec<String> = nm_rows.iter().map(|r| r.3.clone()).collect();

    let demangled = if let Some(cppfilt) = cfg.cppfilt_path {
        demangle_batch(&mangled, cppfilt).unwrap_or_else(|e| {
            tracing::warn!("c++filt unavailable ({e}); falling back to mangled names");
            mangled.clone()
        })
    } else {
        mangled.clone()
    };

    let (ranges, cref_map) = if let Some(map_path) = cfg.map_path {
        match std::fs::read_to_string(map_path) {
            Ok(text) => (parse_linker_map(&text), parse_cref_table(&text)),
            Err(e) => {
                tracing::warn!(
                    "could not read map file {}: {e}; archive attribution and \
                     referenced_by will be unavailable",
                    map_path.display()
                );
                (Vec::new(), BTreeMap::<String, Vec<SymbolReference>>::new())
            }
        }
    } else {
        (Vec::new(), BTreeMap::<String, Vec<SymbolReference>>::new())
    };

    // Pre-walk the ranges to collect mangled owners for map-derived
    // synthetic symbols (anonymous rodata pools, etc.) and demangle
    // them in the same c++filt batch as the nm names — single
    // subprocess, single threaded-stdin pass.
    let mut nm_covered: BTreeMap<u64, u64> = BTreeMap::new();
    for (addr, size, _, _) in nm_rows.iter() {
        nm_covered.insert(*addr, *size);
    }
    let synth_owners = collect_map_derived_owners(&ranges, &nm_covered);
    let synth_mangled: Vec<String> = synth_owners.iter().map(|(_, m, _)| m.clone()).collect();
    let synth_demangled = if synth_mangled.is_empty() {
        Vec::new()
    } else if let Some(cppfilt) = cfg.cppfilt_path {
        demangle_batch(&synth_mangled, cppfilt).unwrap_or_else(|e| {
            tracing::warn!(
                "c++filt unavailable for synthetic owners ({e}); falling back to mangled names"
            );
            synth_mangled.clone()
        })
    } else {
        synth_mangled.clone()
    };

    let mut map = build_fine_grained_map_with_synth(
        elf_s,
        cfg.map_path.map(|p| p.to_string_lossy().to_string()),
        nm_rows,
        demangled,
        ranges,
        &synth_demangled,
        &cref_map,
    );

    // Strip symbols that nm enumerated but that don't actually consume
    // bytes in the final binary — most importantly linker-script
    // boundary markers (`__StackTop`, `__flash_arduino_end`, ...)
    // whose nm-reported "size" is the address gap to the next symbol
    // and can be multiple gigabytes. The PT_LOAD probe is best-effort;
    // when it fails (corrupt ELF, non-ELF input) we leave the map
    // unfiltered rather than poisoning the report with an error.
    match read_pt_load_regions(cfg.elf_path) {
        Ok(regions) if !regions.is_empty() => map.retain_loaded_symbols(&regions),
        Ok(_) => {
            tracing::warn!(
                "no PT_LOAD segments found in {}; emitting unfiltered symbol report",
                cfg.elf_path.display()
            );
        }
        Err(e) => {
            tracing::warn!(
                "PT_LOAD probe failed for {} ({e}); emitting unfiltered symbol report",
                cfg.elf_path.display()
            );
        }
    }

    // #471: per-symbol forward edges from `objdump -d`. When the
    // analyzer was wired with an objdump path (typically from
    // build_info.json::objdump_path), run it once on the linked ELF
    // and pull `<callee>` annotations out of the disassembly. The
    // resulting per-symbol callee map populates each row's
    // `references_to` field, which the bidirectional graph + the
    // dual-ranked callees sub-table consume. Failures are non-fatal
    // — we'd rather ship a report without forward edges than fail
    // the whole symbol-analysis post-link step.
    if let Some(objdump_path) = cfg.objdump_path {
        match run_objdump_and_attribute(objdump_path, cfg.elf_path, &mut map) {
            Ok(edge_count) => {
                tracing::info!(
                    "objdump: extracted {edge_count} forward edges from {}",
                    cfg.elf_path.display()
                );
            }
            Err(e) => {
                tracing::warn!(
                    "objdump forward-edge extraction failed for {} ({e}); \
                     references_to will be empty",
                    cfg.elf_path.display()
                );
            }
        }
    }

    Ok(map)
}

/// Run `objdump -d --no-show-raw-insn <elf>` and populate
/// `references_to` on every symbol in `map` that the parser found
/// outgoing call edges for. Returns the total edge count surfaced
/// (across all symbols) so the caller can log a one-liner.
fn run_objdump_and_attribute(
    objdump_path: &Path,
    elf_path: &Path,
    map: &mut FineGrainedSymbolMap,
) -> Result<usize> {
    use fbuild_core::subprocess::run_command;
    use fbuild_core::symbol_analysis::callgraph::parse_disasm;

    let objdump_s = objdump_path.to_string_lossy().to_string();
    let elf_s = elf_path.to_string_lossy().to_string();
    let args = [
        objdump_s.as_str(),
        "-d",
        "--no-show-raw-insn",
        elf_s.as_str(),
    ];
    let result = run_command(&args, None, None, None)?;
    if !result.success() {
        return Err(FbuildError::BuildFailed(format!(
            "objdump exit={}: {}",
            result.exit_code, result.stderr
        )));
    }

    let edges = parse_disasm(&result.stdout);
    let mut total = 0usize;
    for sym in &mut map.symbols {
        if let Some(callees) = edges.get(&sym.mangled) {
            sym.references_to = callees.clone();
            total += callees.len();
        } else if let Some(callees) = edges.get(&sym.demangled) {
            // Some toolchains demangle in-place when emitting the
            // disassembly, so the function header uses the demangled
            // name. Match against either.
            sym.references_to = callees.clone();
            total += callees.len();
        }
    }
    Ok(total)
}

/// Format a fine-grained per-symbol map as a human-readable text report
/// suitable for streaming to a terminal or stashing in a log artifact.
/// Shows the top `top_n` Flash symbols and top `top_n` Ram symbols with
/// archive + object + section attribution and demangled names.
pub fn format_text_report(map: &FineGrainedSymbolMap, top_n: usize) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "=== Fine-grained symbol analysis: {} ===",
        map.elf_path
    ));
    if let Some(ref m) = map.map_path {
        lines.push(format!("Map file: {m}"));
    }
    lines.push(format!(
        "Flash: {} B across {} sized symbols",
        map.total_flash,
        map.symbols
            .iter()
            .filter(|s| s.region == fbuild_core::MemoryRegion::Flash)
            .count()
    ));
    lines.push(format!(
        "RAM:   {} B across {} sized symbols",
        map.total_ram,
        map.symbols
            .iter()
            .filter(|s| s.region == fbuild_core::MemoryRegion::Ram)
            .count()
    ));
    lines.push(String::new());

    fn emit_region_block(
        lines: &mut Vec<String>,
        map: &FineGrainedSymbolMap,
        region: fbuild_core::MemoryRegion,
        top_n: usize,
        title: &str,
    ) {
        let mut syms: Vec<_> = map.symbols.iter().filter(|s| s.region == region).collect();
        syms.sort_by(|a, b| b.size.cmp(&a.size));
        lines.push(format!(
            "--- Top {} {title} symbols ---",
            top_n.min(syms.len())
        ));
        lines.push(format!(
            "{:>8}  {:<24}  {:<28}  {:<14}  symbol",
            "BYTES", "ARCHIVE", "OBJECT", "SECTION"
        ));
        for s in syms.into_iter().take(top_n) {
            let archive = s.archive.as_deref().unwrap_or("-");
            let object = s.object.as_deref().unwrap_or("-");
            let sect = s.output_section.as_deref().unwrap_or("-");
            // Truncate long demangled names so the table stays scannable.
            let mut name = s.demangled.clone();
            if name.len() > 100 {
                name.truncate(97);
                name.push_str("...");
            }
            lines.push(format!(
                "{:>8}  {:<24}  {:<28}  {:<14}  {}",
                s.size,
                truncate(archive, 24),
                truncate(object, 28),
                truncate(sect, 14),
                name
            ));
        }
        lines.push(String::new());
    }

    emit_region_block(
        &mut lines,
        map,
        fbuild_core::MemoryRegion::Flash,
        top_n,
        "FLASH",
    );
    emit_region_block(
        &mut lines,
        map,
        fbuild_core::MemoryRegion::Ram,
        top_n,
        "RAM",
    );

    // Per-archive roll-ups (flash only — biggest leverage for bloat).
    let mut by_archive: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for s in &map.symbols {
        if s.region != fbuild_core::MemoryRegion::Flash {
            continue;
        }
        let key = s.archive.clone().unwrap_or_else(|| "(unattributed)".into());
        *by_archive.entry(key).or_insert(0) += s.size;
    }
    let mut rows: Vec<(String, u64)> = by_archive.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    lines.push("--- Flash bytes by archive ---".to_string());
    lines.push(format!("{:>10}  ARCHIVE", "BYTES"));
    for (archive, bytes) in rows.into_iter().take(top_n) {
        lines.push(format!("{:>10}  {archive}", bytes));
    }

    lines.join("\n")
}

/// Knobs controlling whether `format_markdown_report` embeds inline
/// Graphviz `.dot` blocks for the top symbols and how the walker
/// behaves while building them. `enabled = false` reproduces the
/// pre-#463 markdown shape byte-for-byte.
#[derive(Debug, Clone)]
pub struct MarkdownGraphOptions {
    /// Embed `.dot` fenced blocks under `<details>` summaries for
    /// the top symbols.
    pub enabled: bool,
    /// How many top symbols (by flash bytes) get an embedded graph.
    /// Capped further by the report's own `top_n`.
    pub graph_top: usize,
    /// Walker / rendering configuration for each embedded graph.
    pub config: GraphConfig,
}

impl Default for MarkdownGraphOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            graph_top: 10,
            config: GraphConfig::default(),
        }
    }
}

/// Format the same fine-grained map as Markdown — same content as
/// `format_text_report` but with real table syntax + headers so it
/// renders nicely in GitHub / VS Code / any MD viewer. Designed to
/// be saved as `report.md` alongside `report.json` so humans and
/// scripts can consume the same analysis without diverging.
///
/// Pre-existing surface; defers to
/// [`format_markdown_report_with_graphs`] with graphs disabled. Kept
/// for legacy callers that don't (yet) want the embedded-graph view.
pub fn format_markdown_report(map: &FineGrainedSymbolMap, top_n: usize) -> String {
    format_markdown_report_with_graphs(
        map,
        top_n,
        &MarkdownGraphOptions {
            enabled: false,
            graph_top: 0,
            config: GraphConfig::default(),
        },
    )
}

/// Same as [`format_markdown_report`] but optionally embeds inline
/// Graphviz `.dot` blocks under each top-N symbol entry (fbuild #463).
/// AI-friendly: a single self-contained `report.md` answers "what
/// pulls in X?" without forcing the agent to fetch a sidecar file.
pub fn format_markdown_report_with_graphs(
    map: &FineGrainedSymbolMap,
    top_n: usize,
    graph_opts: &MarkdownGraphOptions,
) -> String {
    let mut out = String::new();
    use std::fmt::Write as _;
    let flash_count = map
        .symbols
        .iter()
        .filter(|s| s.region == fbuild_core::MemoryRegion::Flash)
        .count();
    let ram_count = map
        .symbols
        .iter()
        .filter(|s| s.region == fbuild_core::MemoryRegion::Ram)
        .count();
    let _ = writeln!(out, "# Symbol analysis: `{}`", map.elf_path);
    let _ = writeln!(out);
    if let Some(ref m) = map.map_path {
        let _ = writeln!(out, "- **Map file**: `{m}`");
    }
    let _ = writeln!(
        out,
        "- **Flash**: {} B across {} sized symbols",
        map.total_flash, flash_count
    );
    let _ = writeln!(
        out,
        "- **RAM**: {} B across {} sized symbols",
        map.total_ram, ram_count
    );
    let _ = writeln!(out);

    fn emit_md_table(
        out: &mut String,
        map: &FineGrainedSymbolMap,
        region: fbuild_core::MemoryRegion,
        top_n: usize,
        title: &str,
    ) {
        use std::fmt::Write as _;
        let mut syms: Vec<_> = map.symbols.iter().filter(|s| s.region == region).collect();
        syms.sort_by(|a, b| b.size.cmp(&a.size));
        let _ = writeln!(
            out,
            "## Top {} {} symbols",
            top_n.min(syms.len()),
            title.to_lowercase()
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "| Bytes | Archive | Object | Section | Source | Referenced by | Symbol |"
        );
        let _ = writeln!(out, "|---:|---|---|---|---|---|---|");
        for s in syms.into_iter().take(top_n) {
            let archive = s.archive.as_deref().unwrap_or("(none)");
            let object = s.object.as_deref().unwrap_or("-");
            let sect = s.output_section.as_deref().unwrap_or("-");
            // Pipe-escape the demangled name so it doesn't break MD
            // table parsing (rare but possible with operator overloads).
            let name = s.demangled.replace('|', "\\|");
            let refs = format_referenced_by(&s.referenced_by, 3);
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | `{}` |",
                s.size, archive, object, sect, s.source, refs, name
            );
        }
        let _ = writeln!(out);
    }

    emit_md_table(
        &mut out,
        map,
        fbuild_core::MemoryRegion::Flash,
        top_n,
        "FLASH",
    );
    emit_md_table(&mut out, map, fbuild_core::MemoryRegion::Ram, top_n, "RAM");

    if graph_opts.enabled && graph_opts.graph_top > 0 {
        emit_backref_graph_section(&mut out, map, top_n, graph_opts);
    }

    // Per-archive flash roll-up.
    let mut by_archive: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for s in &map.symbols {
        if s.region != fbuild_core::MemoryRegion::Flash {
            continue;
        }
        let key = s.archive.clone().unwrap_or_else(|| "(unattributed)".into());
        *by_archive.entry(key).or_insert(0) += s.size;
    }
    let mut rows: Vec<(String, u64)> = by_archive.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let _ = writeln!(out, "## Flash bytes by archive");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Bytes | Archive |");
    let _ = writeln!(out, "|---:|---|");
    for (archive, bytes) in rows.into_iter().take(top_n) {
        let _ = writeln!(out, "| {bytes} | {archive} |");
    }

    out
}

/// Render embedded back-reference graphs for the top symbols
/// directly into the markdown body. Each entry lives under a
/// `<details>` so a long top-10 doesn't crowd the report's eye-line
/// for users who only want the table.
fn emit_backref_graph_section(
    out: &mut String,
    map: &FineGrainedSymbolMap,
    top_n: usize,
    graph_opts: &MarkdownGraphOptions,
) {
    use std::fmt::Write as _;
    let mut syms: Vec<_> = map.symbols.iter().collect();
    syms.sort_by(|a, b| b.size.cmp(&a.size));
    let limit = graph_opts.graph_top.min(top_n).min(syms.len());
    if limit == 0 {
        return;
    }
    let _ = writeln!(
        out,
        "## Top {limit} symbol graphs\n\n\
         For each symbol below: a bidirectional `dot` block (callers on \
         the back-edge side, callees on the forward-edge side), plus a \
         dual-ranked \"Top callees\" sub-table. The forward edges come \
         from per-symbol `references_to` (objdump-derived), so the AI \
         can tell what `ClocklessIdf5` actually calls vs. what its \
         sibling symbols call. See fbuild #463 (backref walker) + \
         #471 (forward edges)."
    );
    let _ = writeln!(out);
    let index = TuIndex::build(map);
    // Per-symbol section: use a bidirectional config so the graph
    // surfaces both axes when forward data is available. If
    // references_to is empty on every row (older fbuild build with no
    // objdump in the chain), the bidirectional walker degenerates
    // gracefully into the same picture the backref-only config would
    // have rendered.
    let mut bidir_cfg = graph_opts.config.clone();
    bidir_cfg.direction = Direction::Bidirectional;
    for (rank, s) in syms.iter().take(limit).enumerate() {
        let archive = s.archive.as_deref().unwrap_or("(none)");
        let object = s.object.as_deref().unwrap_or("-");
        let sect = s.output_section.as_deref().unwrap_or("-");
        let _ = writeln!(
            out,
            "### #{} `{}` — {} B",
            rank + 1,
            s.demangled.replace('|', "\\|"),
            s.size
        );
        let _ = writeln!(out, "- **Archive**: `{archive}`");
        let _ = writeln!(out, "- **Object**: `{object}`");
        let _ = writeln!(out, "- **Section**: `{sect}`");
        let _ = writeln!(out, "- **Referenced by**: {} TUs", s.referenced_by.len());
        let _ = writeln!(
            out,
            "- **References (calls)**: {} symbols",
            s.references_to.len()
        );
        let _ = writeln!(out);

        emit_dual_callees_subtable(out, map, s);

        let graph = BackrefGraph::build_with_index(map, &index, &s.mangled, &bidir_cfg);
        let _ = writeln!(out, "<details>");
        let _ = writeln!(
            out,
            "<summary>Bidirectional graph (callers ← root → callees, Graphviz)</summary>"
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "```dot");
        out.push_str(&graph.to_dot());
        let _ = writeln!(out, "```");
        let _ = writeln!(out);
        let _ = writeln!(out, "</details>");
        let _ = writeln!(out);
    }
}

/// Emit the "Top callees" sub-table for one symbol: two side-by-side
/// rankings (by callee flash bytes, by how widely the callee is
/// shared) plus an `(… and N more)` other-bucket row when applicable.
/// This is the data the user asked for in #471 so an AI optimization
/// pass sees a symbol's actual heavy hitters, not just the symbol
/// itself.
fn emit_dual_callees_subtable(
    out: &mut String,
    map: &FineGrainedSymbolMap,
    caller: &fbuild_core::symbol_analysis::FineGrainedSymbol,
) {
    use std::fmt::Write as _;
    if caller.references_to.is_empty() {
        let _ = writeln!(
            out,
            "_No forward-call data for this symbol — either the \
             analyzer wasn't wired with an objdump or the symbol \
             contains no recognised call instructions (data / weak \
             ref / vtable-only dispatch)._"
        );
        let _ = writeln!(out);
        return;
    }
    let (by_size, by_pop, other) = rank_callees_dual(map, caller, 3);
    let _ = writeln!(out, "#### Top callees (dual ranking)");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| # | by callee size (B) | shared × | by callees \
         shared with (×) | size (B) |"
    );
    let _ = writeln!(out, "|---:|---|---:|---|---:|");
    for i in 0..3 {
        let size_cell = by_size
            .get(i)
            .map(|c| format!("`{}` — {} B", c.demangled.replace('|', "\\|"), c.size))
            .unwrap_or_else(|| "—".into());
        let size_shared = by_size
            .get(i)
            .map(|c| c.callers_count.to_string())
            .unwrap_or_else(|| "—".into());
        let pop_cell = by_pop
            .get(i)
            .map(|c| {
                format!(
                    "`{}` — shared ×{}",
                    c.demangled.replace('|', "\\|"),
                    c.callers_count
                )
            })
            .unwrap_or_else(|| "—".into());
        let pop_size = by_pop
            .get(i)
            .map(|c| c.size.to_string())
            .unwrap_or_else(|| "—".into());
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            i + 1,
            size_cell,
            size_shared,
            pop_cell,
            pop_size
        );
    }
    if other > 0 {
        let _ = writeln!(
            out,
            "| — | _(… and {other} more callees, see graph below)_ | | | |"
        );
    }
    let _ = writeln!(out);
}

/// Configuration for [`write_sidecar_dot_files`].
#[derive(Debug, Clone)]
pub struct SidecarOptions {
    pub enabled: bool,
    /// Minimum symbol size (bytes) for a sidecar `.dot` file to be
    /// emitted. Default 256 — keeps the output directory to symbols
    /// that meaningfully contribute to flash.
    pub min_bytes: u64,
    /// Walker / rendering configuration shared with the embedded
    /// markdown graphs.
    pub config: GraphConfig,
}

impl Default for SidecarOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            min_bytes: 256,
            config: GraphConfig::default(),
        }
    }
}

/// Write sidecar `.dot` files for every symbol whose size meets
/// `options.min_bytes`. Files live at `<output_dir>/graphs/<rank>_<sanitized>.dot`
/// where `rank` is the symbol's index in the size-descending order
/// (1-based). Returns the number of files written.
///
/// Best-effort — never fails the whole report just because one
/// filesystem write errored; logs a `tracing::warn!` and moves on.
pub fn write_sidecar_dot_files(
    map: &FineGrainedSymbolMap,
    output_dir: &Path,
    options: &SidecarOptions,
) -> Result<usize> {
    if !options.enabled {
        return Ok(0);
    }
    let graphs_dir = output_dir.join("graphs");
    std::fs::create_dir_all(&graphs_dir).map_err(|e| {
        FbuildError::Io(std::io::Error::new(
            e.kind(),
            format!("create {}: {e}", graphs_dir.display()),
        ))
    })?;
    let mut syms: Vec<_> = map.symbols.iter().collect();
    syms.sort_by(|a, b| b.size.cmp(&a.size));
    let index = TuIndex::build(map);
    let mut written = 0usize;
    for (i, s) in syms.iter().enumerate() {
        if s.size < options.min_bytes {
            continue;
        }
        let rank = i + 1;
        let stem = sanitize_filename(&s.demangled);
        let path = graphs_dir.join(format!("{rank:04}_{stem}.dot"));
        let graph = BackrefGraph::build_with_index(map, &index, &s.mangled, &options.config);
        let dot = graph.to_dot();
        if let Err(e) = std::fs::write(&path, dot) {
            tracing::warn!(
                "sidecar graph {}: write failed ({e}); skipping",
                path.display()
            );
            continue;
        }
        written += 1;
    }
    Ok(written)
}

/// Best-effort discovery of a `firmware.elf` (or any `.elf`) under
/// the given project directory. Looks in conventional locations in
/// priority order:
///   1. `<dir>/build_info.json` or `build_info_<env>.json` and read
///      `prog_path` (PIO / fbuild emitter convention).
///   2. `<dir>/.fbuild/build/**/firmware.elf` (fbuild native output).
///   3. `<dir>/.pio/build/**/firmware.elf` (PlatformIO output).
///   4. Any `*.elf` directly inside `<dir>`.
///
/// Returns the most recently-modified candidate when multiple match.
pub fn discover_elf_in_project(project_dir: &Path) -> Option<PathBuf> {
    // 1. build_info.json
    let mut build_info_candidates: Vec<PathBuf> = vec![project_dir.join("build_info.json")];
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                if name.starts_with("build_info_") && name.ends_with(".json") {
                    build_info_candidates.push(p);
                }
            }
        }
    }
    for bi in build_info_candidates {
        if let Some(elf) = elf_from_build_info(&bi) {
            if elf.exists() {
                return Some(elf);
            }
        }
    }
    // 2. .fbuild and 3. .pio output trees
    for relative in [".fbuild/build", ".pio/build"] {
        let root = project_dir.join(relative);
        if root.exists() {
            if let Some(elf) = newest_elf_under(&root) {
                return Some(elf);
            }
        }
    }
    // 4. directly under project_dir
    newest_elf_under(project_dir)
}

/// Pull `prog_path` out of a PlatformIO-style build_info.json. The
/// outer object is keyed by env name; we accept the first env that
/// has a usable prog_path. Robust to fbuild's flat shape too — if
/// the top-level itself has `prog_path`, return that.
fn elf_from_build_info(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    if let Some(s) = v.get("prog_path").and_then(|x| x.as_str()) {
        return Some(PathBuf::from(s));
    }
    if let Some(obj) = v.as_object() {
        for (_, inner) in obj.iter() {
            if let Some(s) = inner.get("prog_path").and_then(|x| x.as_str()) {
                return Some(PathBuf::from(s));
            }
        }
    }
    None
}

fn newest_elf_under(root: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    walk_for_elf(root, &mut newest, 0);
    newest.map(|(_, p)| p)
}

fn walk_for_elf(dir: &Path, newest: &mut Option<(std::time::SystemTime, PathBuf)>, depth: usize) {
    // Don't recurse forever — 6 levels is enough for both
    // PIO (.pio/build/<env>/firmware.elf) and fbuild
    // (.fbuild/build/<env>/firmware.elf) layouts.
    if depth > 6 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk_for_elf(&p, newest, depth + 1);
        } else if p.extension().and_then(|e| e.to_str()) == Some("elf") {
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    match newest {
                        Some((cur, _)) if *cur >= mtime => {}
                        _ => *newest = Some((mtime, p)),
                    }
                }
            }
        }
    }
}

/// Format up to `top_k` `referenced_by` entries for a Markdown table
/// cell. Each referencer is rendered as `archive(object)` (or just
/// `object` for bare TUs with no archive) and joined with `, `. When
/// the list exceeds `top_k`, append ` (… and N more)`. Returns `-`
/// for an empty list so the column stays scannable.
///
/// `top_k = 3` is the column-friendly default — the issue proposes
/// K=5 as a follow-up-table value, but five `lib.a(obj.o)` strings
/// per row makes the GitHub-rendered table awkward. Three keeps the
/// signal-to-width ratio readable while still surfacing the most
/// common "libc internal wrapper escapes to an ESP-IDF/mbedTLS TU"
/// pattern documented in #459.
fn format_referenced_by(
    refs: &[fbuild_core::symbol_analysis::SymbolReference],
    top_k: usize,
) -> String {
    if refs.is_empty() {
        return "-".to_string();
    }
    let mut parts: Vec<String> = refs
        .iter()
        .take(top_k)
        .map(|r| match &r.archive {
            Some(a) => format!("{a}({})", r.object),
            None => r.object.clone(),
        })
        .collect();
    if refs.len() > top_k {
        parts.push(format!("(… and {} more)", refs.len() - top_k));
    }
    // Pipe-escape so the joined string doesn't break MD table cells.
    parts.join(", ").replace('|', "\\|")
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max <= 3 {
        ".".repeat(max)
    } else {
        let mut t = s[..max - 3].to_string();
        t.push_str("...");
        t
    }
}

#[cfg(test)]
mod tests {
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
                elf_path.to_string_lossy().replace('\\', "/")
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
        use fbuild_core::symbol_analysis::{FineGrainedSymbol, FineGrainedSymbolMap, SectionBytes};
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
}

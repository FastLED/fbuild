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
use fbuild_core::symbol_analysis::{
    build_fine_grained_map_with_synth, collect_map_derived_owners, parse_cref_table,
    parse_linker_map, parse_nm_output, FineGrainedSymbolMap, LoadedRegion, SymbolReference,
};
use fbuild_core::{FbuildError, Result};

pub mod markdown;

#[cfg(test)]
mod tests;

// Re-export the markdown-side public surface so existing call sites
// (`fbuild_build::symbol_analyzer::format_markdown_report`, etc.)
// keep resolving unchanged after the split.
pub use markdown::{
    format_markdown_report, format_markdown_report_with_graphs, write_sidecar_dot_files,
    MarkdownGraphOptions, SidecarOptions,
};

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
pub async fn analyze_elf(cfg: AnalyzeConfig<'_>) -> Result<FineGrainedSymbolMap> {
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
    let result = run_command(&args, None, None, None).await?;
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
        match run_objdump_and_attribute(objdump_path, cfg.elf_path, &mut map).await {
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
async fn run_objdump_and_attribute(
    objdump_path: &Path,
    elf_path: &Path,
    map: &mut FineGrainedSymbolMap,
) -> Result<usize> {
    use fbuild_core::subprocess::run_command;
    use fbuild_core::symbol_analysis::callgraph::{invert, parse_disasm};

    let objdump_s = objdump_path.to_string_lossy().to_string();
    let elf_s = elf_path.to_string_lossy().to_string();
    let args = [
        objdump_s.as_str(),
        "-d",
        "--no-show-raw-insn",
        elf_s.as_str(),
    ];
    let result = run_command(&args, None, None, None).await?;
    if !result.success() {
        return Err(FbuildError::BuildFailed(format!(
            "objdump exit={}: {}",
            result.exit_code, result.stderr
        )));
    }

    let edges = parse_disasm(&result.stdout);
    // #478: invert once so both per-symbol directions come from the
    // same disassembly pass. `called_by[X]` = every symbol whose
    // forward edge list contains X — the per-symbol-precision view
    // that complements the TU-level `referenced_by` (cref-derived).
    let backward = invert(&edges);
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
        if let Some(callers) = backward.get(&sym.mangled) {
            sym.called_by = callers.clone();
        } else if let Some(callers) = backward.get(&sym.demangled) {
            sym.called_by = callers.clone();
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

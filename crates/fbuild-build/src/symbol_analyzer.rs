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

use fbuild_core::subprocess::run_command_with_stdin;
use fbuild_core::symbol_analysis::{
    build_fine_grained_map, parse_linker_map, parse_nm_output, FineGrainedSymbolMap,
};
use fbuild_core::{FbuildError, Result};

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

    let ranges = if let Some(map_path) = cfg.map_path {
        match std::fs::read_to_string(map_path) {
            Ok(text) => parse_linker_map(&text),
            Err(e) => {
                tracing::warn!(
                    "could not read map file {}: {e}; archive attribution will be unavailable",
                    map_path.display()
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    Ok(build_fine_grained_map(
        elf_s,
        cfg.map_path.map(|p| p.to_string_lossy().to_string()),
        nm_rows,
        demangled,
        ranges,
    ))
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

/// Format the same fine-grained map as Markdown — same content as
/// `format_text_report` but with real table syntax + headers so it
/// renders nicely in GitHub / VS Code / any MD viewer. Designed to
/// be saved as `report.md` alongside `report.json` so humans and
/// scripts can consume the same analysis without diverging.
pub fn format_markdown_report(map: &FineGrainedSymbolMap, top_n: usize) -> String {
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
        let _ = writeln!(out, "| Bytes | Archive | Object | Section | Symbol |");
        let _ = writeln!(out, "|---:|---|---|---|---|");
        for s in syms.into_iter().take(top_n) {
            let archive = s.archive.as_deref().unwrap_or("(none)");
            let object = s.object.as_deref().unwrap_or("-");
            let sect = s.output_section.as_deref().unwrap_or("-");
            // Pipe-escape the demangled name so it doesn't break MD
            // table parsing (rare but possible with operator overloads).
            let name = s.demangled.replace('|', "\\|");
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | `{}` |",
                s.size, archive, object, sect, name
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

/// Best-effort discovery of a `firmware.elf` (or any `.elf`) under
/// the given project directory. Looks in conventional locations in
/// priority order:
///   1. `<dir>/build_info.json` or `build_info_<env>.json` and read
///      `prog_path` (PIO / fbuild emitter convention).
///   2. `<dir>/.fbuild/build/**/firmware.elf` (fbuild native output).
///   3. `<dir>/.pio/build/**/firmware.elf` (PlatformIO output).
///   4. Any `*.elf` directly inside `<dir>`.
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
        assert!(md.contains("| 100 | libA.a | foo.o | .flash.text | `foo(int)` |"));
        assert!(md.contains("## Top 1 ram symbols"));
        assert!(md.contains("| 50 | libB.a | bar.o | .dram0.bss | `bar()` |"));
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
            }],
            sections: Vec::<SectionBytes>::new(),
        };
        let md = format_markdown_report(&map, 5);
        assert!(md.contains("operator\\|(int const&, int const&)"));
    }
}

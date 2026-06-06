//! Driver around `fbuild_core::symbol_analysis` — invokes the cross
//! toolchain (`nm`, `c++filt`) and the linker map alongside an ELF and
//! emits a fully-attributed `FineGrainedSymbolMap`.
//!
//! Designed to work on **any** ELF, including binaries produced by an
//! out-of-band builder (PlatformIO, esp-idf, manual). Drives the same
//! analysis the `fbuild build --symbol-analysis` flag invokes
//! post-link, but is also exposed via the standalone
//! `fbuild symbols <elf>` subcommand.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
/// Spawns a separate writer thread for stdin while the main thread
/// drains stdout via `wait_with_output`. Without that split, the
/// Windows pipe buffer (typically 4-8 KB) deadlocks once c++filt has
/// produced enough output that it stops reading our stdin, but we are
/// still trying to push the rest of the input in. Result stays
/// parallel to the input — when c++filt can't decode a name it echoes
/// it back unchanged, which is the desired fallback.
pub fn demangle_batch(mangled: &[String], cppfilt_path: &Path) -> Result<Vec<String>> {
    if mangled.is_empty() {
        return Ok(Vec::new());
    }
    let stdin_data = mangled.join("\n");

    let mut child = Command::new(cppfilt_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            FbuildError::BuildFailed(format!(
                "failed to spawn c++filt at {}: {e}",
                cppfilt_path.display()
            ))
        })?;

    // Move stdin into a writer thread so stdout draining can happen
    // concurrently in the main thread (avoids the pipe-buffer deadlock).
    let stdin_handle = child.stdin.take().ok_or_else(|| {
        FbuildError::BuildFailed("c++filt stdin pipe unavailable".to_string())
    })?;
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        let mut stdin = stdin_handle;
        stdin.write_all(stdin_data.as_bytes())?;
        // Dropping stdin closes the pipe so c++filt sees EOF.
        Ok(())
    });

    let output = child
        .wait_with_output()
        .map_err(|e| FbuildError::BuildFailed(format!("c++filt wait failed: {e}")))?;
    // Join the writer; any I/O error here usually indicates c++filt
    // closed its stdin early, which is OK as long as the exit was clean.
    let _ = writer
        .join()
        .map_err(|_| FbuildError::BuildFailed("c++filt writer thread panicked".into()))?;
    if !output.status.success() {
        return Err(FbuildError::BuildFailed(format!(
            "c++filt failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut demangled: Vec<String> = stdout.lines().map(|s| s.to_string()).collect();
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
    lines.push(format!("=== Fine-grained symbol analysis: {} ===", map.elf_path));
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
        lines.push(format!("--- Top {} {title} symbols ---", top_n.min(syms.len())));
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
    emit_region_block(&mut lines, map, fbuild_core::MemoryRegion::Ram, top_n, "RAM");

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
            cppfilt.to_string_lossy().ends_with("xtensa-esp32s3-elf-c++filt.exe"),
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
}

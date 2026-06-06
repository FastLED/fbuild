//! `fbuild symbols` — standalone fine-grained per-symbol bloat report.
//!
//! Accepts either an ELF or a project directory; runs the same
//! analysis the build orchestrator's `--symbol-analysis` flag emits,
//! but on any ELF the user points at — including one built by
//! PlatformIO or another out-of-band tool.

use std::path::{Path, PathBuf};

use fbuild_build::symbol_analyzer::{
    analyze_elf, default_map_path, derive_cppfilt_path, discover_elf_in_project,
    format_markdown_report, format_text_report, AnalyzeConfig,
};
use fbuild_core::{FbuildError, Result};

pub fn run_symbols(
    input: String,
    map: Option<String>,
    nm: Option<String>,
    cppfilt: Option<String>,
    json_out: Option<String>,
    output_dir: Option<String>,
    top: usize,
) -> Result<()> {
    let input_path = PathBuf::from(&input);
    if !input_path.exists() {
        return Err(FbuildError::BuildFailed(format!(
            "input not found: {}",
            input_path.display()
        )));
    }

    let elf_path = resolve_elf(&input_path)?;

    let nm_path = match nm {
        Some(p) => PathBuf::from(p),
        None => find_nm_on_path()?,
    };
    if !nm_path.exists() {
        return Err(FbuildError::BuildFailed(format!(
            "nm not found at {}",
            nm_path.display()
        )));
    }

    let cppfilt_path = cppfilt.map(PathBuf::from).or_else(|| {
        let derived = derive_cppfilt_path(&nm_path);
        if derived.exists() {
            Some(derived)
        } else {
            None
        }
    });

    let map_path_owned = map
        .map(PathBuf::from)
        .or_else(|| default_map_path(&elf_path));
    let map_path_ref: Option<&Path> = map_path_owned.as_deref();

    let cfg = AnalyzeConfig {
        elf_path: &elf_path,
        map_path: map_path_ref,
        nm_path: &nm_path,
        cppfilt_path: cppfilt_path.as_deref(),
    };

    let report = analyze_elf(cfg)?;

    let mut wrote_anything = false;

    if let Some(json_path) = json_out {
        write_json(&report, &json_path)?;
        println!(
            "Wrote {} symbols to {} (flash={} B, ram={} B)",
            report.symbols.len(),
            json_path,
            report.total_flash,
            report.total_ram
        );
        wrote_anything = true;
    }

    if let Some(dir_str) = output_dir {
        let dir = PathBuf::from(&dir_str);
        std::fs::create_dir_all(&dir).map_err(|e| {
            FbuildError::Io(std::io::Error::new(
                e.kind(),
                format!("create {dir_str}: {e}"),
            ))
        })?;
        let json_target = dir.join("report.json");
        let md_target = dir.join("report.md");
        write_json(&report, &json_target.to_string_lossy())?;
        let md = format_markdown_report(&report, top);
        std::fs::write(&md_target, md).map_err(|e| {
            FbuildError::Io(std::io::Error::new(
                e.kind(),
                format!("write {}: {e}", md_target.display()),
            ))
        })?;
        println!(
            "Wrote {} symbols to {} and {} (flash={} B, ram={} B)",
            report.symbols.len(),
            json_target.display(),
            md_target.display(),
            report.total_flash,
            report.total_ram
        );
        wrote_anything = true;
    }

    if !wrote_anything {
        println!("{}", format_text_report(&report, top));
    }

    Ok(())
}

fn write_json(
    report: &fbuild_core::symbol_analysis::FineGrainedSymbolMap,
    json_path: &str,
) -> Result<()> {
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| FbuildError::Other(format!("json serialize: {e}")))?;
    std::fs::write(json_path, json).map_err(|e| {
        FbuildError::Io(std::io::Error::new(
            e.kind(),
            format!("write {json_path}: {e}"),
        ))
    })?;
    Ok(())
}

/// Map the CLI input (either an ELF file or a project directory) to
/// an ELF path. Directory inputs go through
/// `discover_elf_in_project` which honours build_info.json,
/// `.fbuild/build/*/firmware.elf`, `.pio/build/*/firmware.elf`, and
/// loose `.elf` files directly inside the directory.
fn resolve_elf(input: &Path) -> Result<PathBuf> {
    if input.is_dir() {
        discover_elf_in_project(input).ok_or_else(|| {
            FbuildError::BuildFailed(format!(
                "no ELF found under {} (looked for build_info.json's \
                 prog_path, .fbuild/build/**/firmware.elf, \
                 .pio/build/**/firmware.elf, and *.elf at top level)",
                input.display()
            ))
        })
    } else {
        Ok(input.to_path_buf())
    }
}

/// Locate `nm` on PATH. The user can always override with `--nm`.
fn find_nm_on_path() -> Result<PathBuf> {
    let exe_name = if cfg!(windows) { "nm.exe" } else { "nm" };
    let path = std::env::var_os("PATH").ok_or_else(|| FbuildError::Other("PATH not set".into()))?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(exe_name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(FbuildError::BuildFailed(format!(
        "{exe_name} not found on PATH; pass --nm to point at a cross toolchain nm"
    )))
}

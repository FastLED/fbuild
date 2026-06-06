//! `fbuild symbols` — standalone fine-grained per-symbol bloat report.
//!
//! Runs the same analysis the build orchestrator's `--symbol-analysis`
//! flag emits, but on any ELF the user points at — including one built
//! by PlatformIO or another out-of-band tool.

use std::path::{Path, PathBuf};

use fbuild_build::symbol_analyzer::{
    analyze_elf, default_map_path, derive_cppfilt_path, format_text_report, AnalyzeConfig,
};
use fbuild_core::{FbuildError, Result};

pub fn run_symbols(
    elf: String,
    map: Option<String>,
    nm: Option<String>,
    cppfilt: Option<String>,
    json_out: Option<String>,
    top: usize,
) -> Result<()> {
    let elf_path = PathBuf::from(elf);
    if !elf_path.exists() {
        return Err(FbuildError::BuildFailed(format!(
            "ELF not found: {}",
            elf_path.display()
        )));
    }

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

    if let Some(json_path) = json_out {
        let json = serde_json::to_string_pretty(&report)
            .map_err(|e| FbuildError::Other(format!("json serialize: {e}")))?;
        std::fs::write(&json_path, json).map_err(|e| {
            FbuildError::Io(std::io::Error::new(
                e.kind(),
                format!("write {json_path}: {e}"),
            ))
        })?;
        println!(
            "Wrote {} symbols to {} (flash={} B, ram={} B)",
            report.symbols.len(),
            json_path,
            report.total_flash,
            report.total_ram
        );
    } else {
        println!("{}", format_text_report(&report, top));
    }

    Ok(())
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

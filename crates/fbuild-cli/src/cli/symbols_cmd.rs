//! `fbuild symbols` — standalone fine-grained per-symbol bloat report.
//!
//! Accepts either an ELF or a project directory; runs the same
//! analysis the build orchestrator's `--symbol-analysis` flag emits,
//! but on any ELF the user points at — including one built by
//! PlatformIO or another out-of-band tool.
//!
//! Toolchain resolution (see #428):
//!   1. `--nm` / `--cppfilt` CLI flags (user wins).
//!   2. `--build-info <path>` if provided — `nm_path` / `cppfilt_path`
//!      read from that file.
//!   3. Auto-discovery: walk up from the ELF directory looking for
//!      `build_info.json` or `build_info_<env>.json`.
//!   4. PATH-based lookup of `nm`, with `c++filt` derived by stem.
//!   5. Hard error.

use std::path::{Path, PathBuf};

use fbuild_build::build_info::{find_build_info_near, load_build_info};
use fbuild_build::symbol_analyzer::{
    analyze_elf, default_map_path, derive_cppfilt_path, discover_elf_in_project,
    format_markdown_report, format_markdown_report_with_graphs, format_text_report,
    write_sidecar_dot_files, AnalyzeConfig, MarkdownGraphOptions, SidecarOptions,
};
use fbuild_core::{FbuildError, Result};

use super::graph_cmd::parse_graph_config;

#[allow(clippy::too_many_arguments)]
pub fn run_symbols(
    input: String,
    map: Option<String>,
    nm: Option<String>,
    cppfilt: Option<String>,
    build_info: Option<String>,
    json_out: Option<String>,
    output_dir: Option<String>,
    top: usize,
    no_graph: bool,
    graph_top: usize,
    graph_min_bytes: u64,
    graph_depth: String,
    graph_fan_out: usize,
    graph_collapse_archive: String,
    graph_exclude_archive: String,
) -> Result<()> {
    let input_path = PathBuf::from(&input);
    if !input_path.exists() {
        return Err(FbuildError::BuildFailed(format!(
            "input not found: {}",
            input_path.display()
        )));
    }

    let elf_path = resolve_elf(&input_path)?;

    let tool_paths = ToolPaths::resolve(
        &elf_path,
        nm.as_deref(),
        cppfilt.as_deref(),
        build_info.as_deref(),
    )?;
    let nm_path = tool_paths.nm;
    let cppfilt_path = tool_paths.cppfilt;
    if !nm_path.exists() {
        return Err(FbuildError::BuildFailed(format!(
            "nm not found at {}\n\
             Resolution searched: --nm flag → --build-info → \
             build_info.json near ELF → PATH.\n\
             Pass --nm explicitly to point at a cross-toolchain nm,\n\
             or run `fbuild build` first so build_info.json carries nm_path.",
            nm_path.display()
        )));
    }

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
        let graph_config = parse_graph_config(
            &graph_depth,
            graph_fan_out,
            /*max_depth=*/ 4,
            &graph_collapse_archive,
            &graph_exclude_archive,
        )?;
        let md = if no_graph {
            format_markdown_report(&report, top)
        } else {
            format_markdown_report_with_graphs(
                &report,
                top,
                &MarkdownGraphOptions {
                    enabled: true,
                    graph_top,
                    config: graph_config.clone(),
                },
            )
        };
        std::fs::write(&md_target, md).map_err(|e| {
            FbuildError::Io(std::io::Error::new(
                e.kind(),
                format!("write {}: {e}", md_target.display()),
            ))
        })?;
        let sidecar_count = if no_graph {
            0
        } else {
            write_sidecar_dot_files(
                &report,
                &dir,
                &SidecarOptions {
                    enabled: true,
                    min_bytes: graph_min_bytes,
                    config: graph_config,
                },
            )?
        };
        println!(
            "Wrote {} symbols to {} and {} (flash={} B, ram={} B); {} sidecar graphs",
            report.symbols.len(),
            json_target.display(),
            md_target.display(),
            report.total_flash,
            report.total_ram,
            sidecar_count,
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

/// Resolved toolchain paths for the symbol analyzer.
struct ToolPaths {
    nm: PathBuf,
    cppfilt: Option<PathBuf>,
}

impl ToolPaths {
    /// Resolve `nm` / `c++filt` using the precedence documented in the
    /// module header. `build_info_arg` is the explicit `--build-info`
    /// path; when absent, walk up from `elf_path`.
    fn resolve(
        elf_path: &Path,
        nm: Option<&str>,
        cppfilt: Option<&str>,
        build_info_arg: Option<&str>,
    ) -> Result<Self> {
        // Try the build_info source (explicit flag wins over auto-discovery).
        let build_info_path = build_info_arg
            .map(PathBuf::from)
            .or_else(|| elf_path.parent().and_then(find_build_info_near));

        let (bi_nm, bi_cppfilt) = match build_info_path {
            Some(path) => match load_build_info(&path) {
                Ok((_env, info)) => {
                    tracing::info!("symbols: read toolchain paths from {}", path.display());
                    (option_path(&info.nm_path), option_path(&info.cppfilt_path))
                }
                Err(e) => {
                    tracing::warn!(
                        "symbols: ignoring {}: {} (falling back to PATH)",
                        path.display(),
                        e
                    );
                    (None, None)
                }
            },
            None => (None, None),
        };

        let nm = match nm {
            Some(p) => PathBuf::from(p),
            None => match bi_nm {
                Some(p) => p,
                None => find_nm_on_path()?,
            },
        };

        let cppfilt = match cppfilt {
            Some(p) => Some(PathBuf::from(p)),
            None => bi_cppfilt.or_else(|| {
                let derived = derive_cppfilt_path(&nm);
                if derived.exists() {
                    Some(derived)
                } else {
                    None
                }
            }),
        };

        Ok(Self { nm, cppfilt })
    }
}

/// Public wrapper around the internal toolchain resolver — used by
/// `fbuild bloat graph` (`graph_cmd.rs`) so it shares the exact
/// `--nm` / `--cppfilt` / `--build-info` resolution semantics as
/// `fbuild symbols`. Returns `(nm_path, optional_cppfilt_path)`.
pub fn resolve_tool_paths_public(
    elf_path: &Path,
    nm: Option<&str>,
    cppfilt: Option<&str>,
    build_info_arg: Option<&str>,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let resolved = ToolPaths::resolve(elf_path, nm, cppfilt, build_info_arg)?;
    if !resolved.nm.exists() {
        return Err(FbuildError::BuildFailed(format!(
            "nm not found at {}\n\
             Pass --nm explicitly or run `fbuild build` first so build_info.json carries nm_path.",
            resolved.nm.display()
        )));
    }
    Ok((resolved.nm, resolved.cppfilt))
}

/// Treat an empty BuildInfo path field (the schema's "missing"
/// sentinel) as `None`. `BuildInfo`'s `*_path` fields became
/// `NormalizedPath` in #437 Phase 2, so emptiness is checked on the
/// underlying `OsStr` rather than on a `String`.
fn option_path(p: &fbuild_core::path::NormalizedPath) -> Option<PathBuf> {
    if p.as_path().as_os_str().is_empty() {
        None
    } else {
        Some(p.as_path().to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_build::build_info::{emit_build_info, BuildInfo};

    fn dummy_build_info(nm: &str, cppfilt: &str) -> BuildInfo {
        // size_path drives the four derived tool paths; pretend size has
        // a name that won't match anything on disk so derivation alone
        // doesn't fool the test — we explicitly override nm/cppfilt below.
        let mut info = BuildInfo::new(
            Path::new("/build/firmware.elf"),
            Some(Path::new("/bin/gcc")),
            Some(Path::new("/bin/g++")),
            None,
            None,
            Path::new("/bin/size"),
            vec![],
            vec![],
            vec![],
            vec![],
            "test".to_string(),
            "test".to_string(),
            "test".to_string(),
        );
        info.nm_path = fbuild_core::path::NormalizedPath::new(nm);
        info.cppfilt_path = fbuild_core::path::NormalizedPath::new(cppfilt);
        info
    }

    /// #428: when `build_info.json` lives near the ELF and carries
    /// `nm_path`, the symbols CLI must pick it up automatically.
    #[test]
    fn resolve_reads_nm_from_build_info_auto_discovery() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path();
        let build_dir = project.join(".fbuild").join("build").join("uno");
        std::fs::create_dir_all(&build_dir).unwrap();
        let elf = build_dir.join("firmware.elf");
        std::fs::write(&elf, b"\x7fELF").unwrap();

        // We point nm_path at a file that actually exists on disk so the
        // resolver doesn't later trip the existence check.
        let nm_file = project.join("fake-nm");
        std::fs::write(&nm_file, b"#!/bin/false\n").unwrap();
        let info = dummy_build_info(&nm_file.to_string_lossy(), "");
        emit_build_info(project, "uno", &info).unwrap();

        let tools = ToolPaths::resolve(&elf, None, None, None).unwrap();
        assert_eq!(tools.nm, nm_file);
    }

    /// Explicit `--nm` overrides whatever build_info.json says.
    #[test]
    fn resolve_explicit_nm_wins_over_build_info() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path();
        let elf = project.join("firmware.elf");
        std::fs::write(&elf, b"\x7fELF").unwrap();

        // build_info points at one nm…
        let bi_nm = project.join("from-buildinfo");
        std::fs::write(&bi_nm, b"x").unwrap();
        let info = dummy_build_info(&bi_nm.to_string_lossy(), "");
        emit_build_info(project, "uno", &info).unwrap();

        // …user overrides with --nm pointing at a different one.
        let cli_nm = project.join("from-cli");
        std::fs::write(&cli_nm, b"x").unwrap();

        let tools = ToolPaths::resolve(&elf, Some(cli_nm.to_str().unwrap()), None, None).unwrap();
        assert_eq!(tools.nm, cli_nm);
    }

    /// `--build-info <path>` is honoured even when the ELF isn't under
    /// the project containing build_info.json.
    #[test]
    fn resolve_explicit_build_info_path_is_honoured() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp.path().join("firmware.elf");
        std::fs::write(&elf, b"\x7fELF").unwrap();

        let bi_dir = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&bi_dir).unwrap();
        let nm_file = bi_dir.join("nm-from-explicit");
        std::fs::write(&nm_file, b"x").unwrap();
        let info = dummy_build_info(&nm_file.to_string_lossy(), "");
        emit_build_info(&bi_dir, "uno", &info).unwrap();

        let bi_path = bi_dir.join("build_info.json");
        let tools = ToolPaths::resolve(&elf, None, None, Some(bi_path.to_str().unwrap())).unwrap();
        assert_eq!(tools.nm, nm_file);
    }
}

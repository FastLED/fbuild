//! `fbuild bloat` — standalone fine-grained per-symbol bloat report.
//!
//! Aligns with `cargo bloat` / Google's `bloaty` for vocabulary. The
//! legacy CLI spelling `fbuild symbols` is still accepted via clap
//! alias for back-compat through 2.3.x.
//!
//! Accepts either a project directory (preferred) or an ELF; runs the
//! same analysis the build orchestrator's `--symbol-analysis` flag
//! emits, but on any ELF the user points at — including one built by
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
    format_markdown_report, AnalyzeConfig,
};
use fbuild_core::symbol_analysis::FineGrainedSymbolMap;
use fbuild_core::{FbuildError, MemoryRegion, Result};

#[allow(clippy::too_many_arguments)]
pub fn run_bloat(
    input: String,
    map: Option<String>,
    nm: Option<String>,
    cppfilt: Option<String>,
    build_info: Option<String>,
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

    let resolution = ToolPaths::resolve(
        &elf_path,
        nm.as_deref(),
        cppfilt.as_deref(),
        build_info.as_deref(),
    )?;
    let nm_path = resolution.tools.nm;
    let cppfilt_path = resolution.tools.cppfilt;
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

    // Mode 1: explicit --json only → write JSON, stream text to stdout.
    // Mode 2: explicit --output-dir → write both into that dir.
    // Mode 3: neither → default to `<project>/.fbuild/build/<env>/bloat-report/`
    //         (or `<elf-parent>/bloat-report/` when no build_info found).
    if let Some(json_path) = json_out.as_deref() {
        write_json(&report, json_path)?;
        println!(
            "Wrote {} symbols to {} (flash={} B, ram={} B)",
            report.symbols.len(),
            json_path,
            report.total_flash,
            report.total_ram
        );
        return Ok(());
    }

    let dir = match output_dir {
        Some(s) => PathBuf::from(s),
        None => default_output_dir(&elf_path, resolution.project.as_ref()),
    };
    write_dual_report(&report, &dir, top)?;
    print_bloat_summary(&report, &dir);

    Ok(())
}

/// Print the standard end-of-run summary: a header with totals, then
/// the two absolute paths verbatim — one per line — so tools can
/// `grep -E "report\\.(json|md)"` to pick them up (#439).
fn print_bloat_summary(report: &FineGrainedSymbolMap, dir: &Path) {
    let json_abs = absolute(&dir.join("report.json"));
    let md_abs = absolute(&dir.join("report.md"));
    let flash_count = report
        .symbols
        .iter()
        .filter(|s| s.region == MemoryRegion::Flash)
        .count();
    let ram_count = report
        .symbols
        .iter()
        .filter(|s| s.region == MemoryRegion::Ram)
        .count();
    println!("Bloat report:");
    println!(
        "  Flash: {} B across {} symbols",
        report.total_flash, flash_count
    );
    println!(
        "  RAM:   {} B across {} symbols",
        report.total_ram, ram_count
    );
    println!();
    println!("Written to:");
    println!("  {}", json_abs.display());
    println!("  {}", md_abs.display());
}

/// Default output directory when neither `--json` nor `--output-dir`
/// is given. Prefers the project layout
/// `<project>/.fbuild/build/<env>/bloat-report/` so the report lands
/// next to the build artefacts and is easy to find.
fn default_output_dir(elf_path: &Path, project: Option<&ProjectContext>) -> PathBuf {
    match project {
        Some(p) => p
            .project_dir
            .join(".fbuild")
            .join("build")
            .join(&p.env_name)
            .join("bloat-report"),
        None => elf_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("bloat-report"),
    }
}

/// Best-effort absolutize so the exit lines are predictable for
/// downstream tooling. Falls back to the original path on canonical-
/// isation failure (relative paths still work, just won't be absolute).
fn absolute(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(p))
                .unwrap_or_else(|_| p.to_path_buf())
        }
    })
}

/// Write both `report.json` and `report.md` into `dir`. Creates
/// parents as needed.
fn write_dual_report(report: &FineGrainedSymbolMap, dir: &Path, top: usize) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| {
        FbuildError::Io(std::io::Error::new(
            e.kind(),
            format!("create {}: {e}", dir.display()),
        ))
    })?;
    let json_target = dir.join("report.json");
    let md_target = dir.join("report.md");
    write_json(report, &json_target.to_string_lossy())?;
    let md = format_markdown_report(report, top);
    std::fs::write(&md_target, md).map_err(|e| {
        FbuildError::Io(std::io::Error::new(
            e.kind(),
            format!("write {}: {e}", md_target.display()),
        ))
    })?;
    Ok(())
}

fn write_json(report: &FineGrainedSymbolMap, json_path: &str) -> Result<()> {
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

/// Project layout pulled from `build_info.json`. Drives the default
/// output directory in [`default_output_dir`] (#439).
struct ProjectContext {
    /// Directory containing the `build_info.json` we read from —
    /// typically the project root.
    project_dir: PathBuf,
    /// PIO env name (e.g. `"esp32s3"`) from the build_info's `env`
    /// field. Used as the subdir under `.fbuild/build/`.
    env_name: String,
}

/// Combined output of [`ToolPaths::resolve`]: the toolchain paths the
/// analyzer needs plus, when discovered, the project context for the
/// default-output-dir computation.
struct Resolution {
    tools: ToolPaths,
    project: Option<ProjectContext>,
}

impl ToolPaths {
    /// Resolve `nm` / `c++filt` using the precedence documented in the
    /// module header. `build_info_arg` is the explicit `--build-info`
    /// path; when absent, walk up from `elf_path`. Also returns a
    /// [`ProjectContext`] when a build_info.json was found so the
    /// caller can compute the default output directory.
    fn resolve(
        elf_path: &Path,
        nm: Option<&str>,
        cppfilt: Option<&str>,
        build_info_arg: Option<&str>,
    ) -> Result<Resolution> {
        // Try the build_info source (explicit flag wins over auto-discovery).
        let build_info_path = build_info_arg
            .map(PathBuf::from)
            .or_else(|| elf_path.parent().and_then(find_build_info_near));

        let (bi_nm, bi_cppfilt, project) = match build_info_path {
            Some(path) => match load_build_info(&path) {
                Ok((env_name, info)) => {
                    tracing::info!("bloat: read toolchain paths from {}", path.display());
                    let project = path.parent().map(|p| ProjectContext {
                        project_dir: p.to_path_buf(),
                        // Prefer the env_name from the JSON's outer key
                        // (matches FastLED's `_create_board_info`
                        // contract); fall back to `info.env`.
                        env_name: if !env_name.is_empty() {
                            env_name
                        } else {
                            info.env.clone()
                        },
                    });
                    (
                        option_path(&info.nm_path),
                        option_path(&info.cppfilt_path),
                        project,
                    )
                }
                Err(e) => {
                    tracing::warn!(
                        "bloat: ignoring {}: {} (falling back to PATH)",
                        path.display(),
                        e
                    );
                    (None, None, None)
                }
            },
            None => (None, None, None),
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

        Ok(Resolution {
            tools: Self { nm, cppfilt },
            project,
        })
    }
}

/// Treat an empty BuildInfo string field (the schema's "missing"
/// sentinel) as `None`.
fn option_path(s: &str) -> Option<PathBuf> {
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
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
        info.nm_path = nm.to_string();
        info.cppfilt_path = cppfilt.to_string();
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

        let resolution = ToolPaths::resolve(&elf, None, None, None).unwrap();
        assert_eq!(resolution.tools.nm, nm_file);
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

        let resolution =
            ToolPaths::resolve(&elf, Some(cli_nm.to_str().unwrap()), None, None).unwrap();
        assert_eq!(resolution.tools.nm, cli_nm);
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
        let resolution =
            ToolPaths::resolve(&elf, None, None, Some(bi_path.to_str().unwrap())).unwrap();
        assert_eq!(resolution.tools.nm, nm_file);
    }

    /// #439: `default_output_dir` lands under
    /// `<project>/.fbuild/build/<env>/bloat-report/` when a build_info
    /// project context is present.
    #[test]
    fn default_output_dir_uses_project_layout() {
        let project = ProjectContext {
            project_dir: PathBuf::from("/abs/project"),
            env_name: "esp32s3".to_string(),
        };
        let elf = PathBuf::from("/abs/project/.fbuild/build/esp32s3/firmware.elf");
        let dir = default_output_dir(&elf, Some(&project));
        assert_eq!(
            dir,
            PathBuf::from("/abs/project")
                .join(".fbuild")
                .join("build")
                .join("esp32s3")
                .join("bloat-report")
        );
    }

    /// #439: `default_output_dir` falls back to ELF parent /
    /// `bloat-report/` when no project context was discovered.
    #[test]
    fn default_output_dir_falls_back_to_elf_parent() {
        let elf = PathBuf::from("/tmp/firmware.elf");
        let dir = default_output_dir(&elf, None);
        assert_eq!(dir, PathBuf::from("/tmp").join("bloat-report"));
    }

    /// #439: end-to-end smoke — when build_info.json carries env=uno,
    /// `ToolPaths::resolve` should produce a project context the
    /// default-dir logic can use.
    #[test]
    fn resolve_returns_project_context_when_build_info_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path();
        let build_dir = project.join(".fbuild").join("build").join("uno");
        std::fs::create_dir_all(&build_dir).unwrap();
        let elf = build_dir.join("firmware.elf");
        std::fs::write(&elf, b"\x7fELF").unwrap();
        let nm_file = project.join("fake-nm");
        std::fs::write(&nm_file, b"x").unwrap();
        let info = dummy_build_info(&nm_file.to_string_lossy(), "");
        emit_build_info(project, "uno", &info).unwrap();

        let resolution = ToolPaths::resolve(&elf, None, None, None).unwrap();
        let pc = resolution
            .project
            .expect("project context must be set when build_info.json was found");
        assert_eq!(pc.env_name, "uno");
        // project_dir points at the directory containing build_info.json.
        assert_eq!(pc.project_dir, project);
    }
}

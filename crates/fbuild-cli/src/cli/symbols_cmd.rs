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
//!      or PlatformIO aliases read from that file.
//!   3. Auto-discovery: walk up from the ELF directory looking for
//!      `build_info.json` or `build_info_<env>.json`.
//!   4. PATH-based lookup of `nm`, with `c++filt` derived by stem.
//!   5. Hard error.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use fbuild_build::symbol_analyzer::{
    AnalyzeConfig, MarkdownGraphOptions, SidecarOptions, analyze_elf, default_map_path,
    derive_cppfilt_path, discover_elf_in_project, format_markdown_report,
    format_markdown_report_with_graphs, format_text_report, write_sidecar_dot_files,
};
use fbuild_core::{FbuildError, Result};
use serde::Deserialize;

use crate::output;

use super::graph_cmd::parse_graph_config;

#[allow(clippy::too_many_arguments)]
#[expect(clippy::too_many_lines, reason = "baseline, zackees/ci.yml#229")]
pub async fn run_symbols(
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
    )
    .await?;
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
        objdump_path: tool_paths.objdump.as_deref(),
    };

    let report = analyze_elf(cfg).await?;

    let mut wrote_anything = false;

    if let Some(json_path) = json_out {
        write_json(&report, &json_path).await?;
        output::result(format!(
            "Wrote {} symbols to {} (flash={} B attributed, image_flash={} B, ram={} B)",
            report.symbols.len(),
            json_path,
            report.total_flash,
            report
                .image_flash
                .map_or_else(|| "?".to_string(), |b| b.to_string()),
            report.total_ram
        ));
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
        write_json(&report, &json_target.to_string_lossy()).await?;
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
        output::result(format!(
            "Wrote {} symbols to {} and {} (flash={} B attributed, image_flash={} B, ram={} B); {} sidecar graphs",
            report.symbols.len(),
            json_target.display(),
            md_target.display(),
            report.total_flash,
            report
                .image_flash
                .map_or_else(|| "?".to_string(), |b| b.to_string()),
            report.total_ram,
            sidecar_count,
        ));
        wrote_anything = true;
    }

    if !wrote_anything {
        output::result(format_text_report(&report, top));
    }

    Ok(())
}

async fn write_json(
    report: &fbuild_core::symbol_analysis::FineGrainedSymbolMap,
    json_path: &str,
) -> Result<()> {
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| FbuildError::Other(format!("json serialize: {e}")))?;
    // Atomic write — FastLED/fbuild#844 bridge pair 6 (symbol report).
    fbuild_core::fs::write_atomic(json_path, json)
        .await
        .map_err(|e| {
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
                "no ELF found under {} (looked for build_info.json's prog_path, \
                 {}/{}/**/firmware.elf, .pio/build/**/firmware.elf, and *.elf at \
                 top level)",
                input.display(),
                fbuild_paths::FBUILD_DIR_NAME,
                fbuild_paths::BUILD_DIR_NAME
            ))
        })
    } else {
        Ok(input.to_path_buf())
    }
}

/// Locate `nm` on PATH. The user can always override with `--nm`.
fn find_nm_on_path() -> Result<PathBuf> {
    let exe_name = fbuild_core::platform::executable::native_name("nm");
    let path = std::env::var_os("PATH").ok_or_else(|| FbuildError::Other("PATH not set".into()))?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(&exe_name);
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
    /// `objdump`, when discoverable. Populated from build_info.json
    /// (`objdump_path`) or by deriving it from the nm path using the
    /// GCC cross-tool naming convention. `None` when neither path
    /// can be found — the analyzer falls back to an empty
    /// `references_to` (instruction references are unavailable; map
    /// object references are unaffected).
    objdump: Option<PathBuf>,
}

impl ToolPaths {
    /// Resolve `nm` / `c++filt` / `objdump` using the precedence
    /// documented in the module header. `build_info_arg` is the
    /// explicit `--build-info` path; when absent, walk up from
    /// `elf_path`.
    async fn resolve(
        elf_path: &Path,
        nm: Option<&str>,
        cppfilt: Option<&str>,
        build_info_arg: Option<&str>,
    ) -> Result<Self> {
        let build_info_path = match build_info_arg {
            Some(path) => Some(PathBuf::from(path)),
            None if nm.is_some() => None,
            None => discover_tool_metadata(elf_path)?,
        };
        let (bi_nm, bi_cppfilt, bi_objdump) = match build_info_path {
            Some(path) => {
                let info = read_tool_metadata(&path, elf_path).await?;
                if nm.is_none() && info.tool("nm", &info.nm_path).is_none() {
                    return Err(metadata_error(
                        &path,
                        "selected environment has no nm_path or aliases.nm; pass --nm",
                    ));
                }
                tracing::info!("symbols: read toolchain paths from {}", path.display());
                (
                    info.tool("nm", &info.nm_path),
                    info.tool("c++filt", &info.cppfilt_path),
                    info.tool("objdump", &info.objdump_path),
                )
            }
            None => (None, None, None),
        };
        let explicit_nm = nm.is_some();

        let nm = match nm {
            Some(p) => PathBuf::from(p),
            None => match bi_nm {
                Some(p) => p,
                None => find_nm_on_path()?,
            },
        };

        let cppfilt = match cppfilt {
            Some(p) => Some(PathBuf::from(p)),
            None => (if explicit_nm { None } else { bi_cppfilt }).or_else(|| {
                let derived = derive_cppfilt_path(&nm);
                if derived.exists() {
                    Some(derived)
                } else {
                    None
                }
            }),
        };

        // objdump: prefer build_info, else derive from nm using the
        // GCC cross-tool prefix (`<prefix>-nm` → `<prefix>-objdump`).
        // Same prefix-replacement strategy `derive_cppfilt_path`
        // uses; inlined here to keep symbol_analyzer's public surface
        // minimal — objdump derivation isn't useful outside this CLI.
        let objdump = if explicit_nm {
            derive_sibling_tool(&nm, "objdump")
        } else {
            bi_objdump.or_else(|| derive_sibling_tool(&nm, "objdump"))
        };

        Ok(Self {
            nm,
            cppfilt,
            objdump,
        })
    }
}

/// Replace the `nm` suffix on the file stem with `target` (e.g.
/// `arm-none-eabi-nm` → `arm-none-eabi-objdump`). Returns `None`
/// when the result doesn't exist on disk — caller treats absent as
/// "skip the feature", not as an error.
fn derive_sibling_tool(nm_path: &Path, target: &str) -> Option<PathBuf> {
    let parent = nm_path.parent().unwrap_or(Path::new("."));
    let stem = nm_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = nm_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let new_stem = if let Some(prefix) = stem.strip_suffix("nm") {
        format!("{prefix}{target}")
    } else {
        target.to_string()
    };
    let candidate = if ext.is_empty() {
        parent.join(new_stem)
    } else {
        parent.join(format!("{new_stem}.{ext}"))
    };
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

/// Public wrapper around the internal toolchain resolver — used by
/// `fbuild bloat graph` (`graph_cmd.rs`) so it shares the exact
/// `--nm` / `--cppfilt` / `--build-info` resolution semantics as
/// `fbuild symbols`. Returns `(nm_path, optional_cppfilt_path,
/// optional_objdump_path)` — the third field added in #471 carries
/// the objdump used to populate per-symbol forward refs
/// (`references_to`). Callers that don't care about forward graphs
/// can discard the third field.
pub async fn resolve_tool_paths_public(
    elf_path: &Path,
    nm: Option<&str>,
    cppfilt: Option<&str>,
    build_info_arg: Option<&str>,
) -> Result<(PathBuf, Option<PathBuf>, Option<PathBuf>)> {
    let resolved = ToolPaths::resolve(elf_path, nm, cppfilt, build_info_arg).await?;
    if !resolved.nm.exists() {
        return Err(FbuildError::BuildFailed(format!(
            "nm not found at {}\n\
             Pass --nm explicitly or run `fbuild build` first so build_info.json carries nm_path.",
            resolved.nm.display()
        )));
    }
    Ok((resolved.nm, resolved.cppfilt, resolved.objdump))
}

/// Project only the tool metadata: PlatformIO aliases do not carry all native
/// BuildInfo fields, and requiring compiler/build flags silently lost graphs.
#[derive(Default, Deserialize)]
struct SymbolToolMetadata {
    #[serde(default)]
    prog_path: String,
    #[serde(default)]
    nm_path: String,
    #[serde(default)]
    cppfilt_path: String,
    #[serde(default)]
    objdump_path: String,
    #[serde(default)]
    aliases: BTreeMap<String, String>,
}

impl SymbolToolMetadata {
    fn tool(&self, alias: &str, direct: &str) -> Option<PathBuf> {
        let value = if direct.is_empty() {
            self.aliases.get(alias).map(String::as_str)?
        } else {
            direct
        };
        (!value.is_empty()).then(|| PathBuf::from(value))
    }
}

fn elf_environment(elf: &Path) -> Option<&str> {
    // Build profiles may add release/debug beneath the environment directory.
    elf.parent()?.ancestors().find_map(|directory| {
        (directory.parent()?.file_name()? == "build")
            .then(|| directory.file_name()?.to_str())
            .flatten()
    })
}

fn metadata_error(path: &Path, reason: impl std::fmt::Display) -> FbuildError {
    FbuildError::BuildFailed(format!(
        "symbols: invalid tool metadata {}: {reason}",
        path.display()
    ))
}

async fn read_tool_metadata(path: &Path, elf: &Path) -> Result<SymbolToolMetadata> {
    let bytes = fbuild_core::fs::read(path)
        .await
        .map_err(|e| metadata_error(path, e))?;
    let mut envs: BTreeMap<String, SymbolToolMetadata> =
        serde_json::from_slice(&bytes).map_err(|e| metadata_error(path, e))?;
    if envs.is_empty() {
        return Err(metadata_error(path, "no environments"));
    }
    let mut matches = Vec::new();
    let canonical_elf = fbuild_core::path::canonicalize_existing(elf).await.ok();
    for (name, info) in &envs {
        if info.prog_path.is_empty() {
            continue;
        }
        let program = Path::new(&info.prog_path);
        let candidate = if program.is_absolute() {
            program.to_path_buf()
        } else {
            path.parent().unwrap_or(Path::new(".")).join(program)
        };
        let same_lexical_path = fbuild_core::path::NormalizedPath::new(program)
            == fbuild_core::path::NormalizedPath::new(elf);
        let same_existing_path = match &canonical_elf {
            Some(elf_identity) => fbuild_core::path::canonicalize_existing(candidate)
                .await
                .ok()
                .is_some_and(|identity| &identity == elf_identity),
            None => false,
        };
        if same_lexical_path || same_existing_path {
            matches.push(name.clone());
        }
    }
    let selected = match matches.as_slice() {
        [name] => name.clone(),
        [] => match elf_environment(elf).filter(|name| envs.contains_key(*name)) {
            Some(name) => name.to_string(),
            None if envs.len() == 1 && elf_environment(elf).is_none() => envs
                .keys()
                .next()
                .cloned()
                .ok_or_else(|| metadata_error(path, "no environments"))?,
            None => {
                return Err(metadata_error(
                    path,
                    "no unambiguous environment matches the ELF; use matching prog_path or build/<env>/firmware.elf",
                ));
            }
        },
        _ => return Err(metadata_error(path, "multiple environments match the ELF")),
    };
    envs.remove(&selected)
        .ok_or_else(|| metadata_error(path, "selected environment is missing"))
}

fn discover_tool_metadata(elf: &Path) -> Result<Option<PathBuf>> {
    let mut cursor = elf.parent();
    while let Some(dir) = cursor {
        if let Some(env) = elf_environment(elf) {
            let matching = dir.join(format!("build_info_{env}.json"));
            if matching.is_file() {
                return Ok(Some(matching));
            }
        }
        let generic = dir.join("build_info.json");
        if generic.is_file() {
            return Ok(Some(generic));
        }
        let mut candidates = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|name| {
                            name.starts_with("build_info_") && name.ends_with(".json")
                        })
                {
                    candidates.push(path);
                }
            }
        }
        match candidates.len() {
            0 => {}
            1 => return Ok(candidates.pop()),
            _ => {
                return Err(metadata_error(
                    dir,
                    "ambiguous build_info_<env>.json files; pass --build-info",
                ));
            }
        }
        cursor = dir.parent();
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_build::build_info::{BuildInfo, emit_build_info};

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
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_reads_nm_from_build_info_auto_discovery() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path();
        let build_dir = fbuild_paths::get_project_fbuild_dir(project)
            .join(fbuild_paths::BUILD_DIR_NAME)
            .join("uno");
        std::fs::create_dir_all(&build_dir).unwrap();
        let elf = build_dir.join("firmware.elf");
        std::fs::write(&elf, b"\x7fELF").unwrap();

        // We point nm_path at a file that actually exists on disk so the
        // resolver doesn't later trip the existence check.
        let nm_file = project.join("fake-nm");
        std::fs::write(&nm_file, b"#!/bin/false\n").unwrap();
        let info = dummy_build_info(&nm_file.to_string_lossy(), "");
        emit_build_info(project, "uno", &info).unwrap();

        let tools = ToolPaths::resolve(&elf, None, None, None).await.unwrap();
        assert_eq!(tools.nm, nm_file);
    }

    /// Explicit `--nm` overrides whatever build_info.json says.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_explicit_nm_wins_over_build_info() {
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

        let tools = ToolPaths::resolve(&elf, Some(cli_nm.to_str().unwrap()), None, None)
            .await
            .unwrap();
        assert_eq!(tools.nm, cli_nm);
    }

    /// `--build-info <path>` is honoured even when the ELF isn't under
    /// the project containing build_info.json.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_explicit_build_info_path_is_honoured() {
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
        let tools = ToolPaths::resolve(&elf, None, None, Some(bi_path.to_str().unwrap()))
            .await
            .unwrap();
        assert_eq!(tools.nm, nm_file);
    }
    #[tokio::test]
    async fn resolve_partial_aliases_and_matching_environment() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp.path().join(".pio/build/uno/firmware.elf");
        let metadata = tmp.path().join("build_info.json");
        std::fs::write(&metadata, serde_json::to_vec(&serde_json::json!({
            "uno": {"aliases": {"nm": "/target/avr-nm", "c++filt": "/target/avr-c++filt", "objdump": "/target/avr-objdump"}},
            "esp": {"aliases": {"nm": "/target/xtensa-nm"}}
        })).unwrap()).unwrap();
        let tools = ToolPaths::resolve(&elf, None, None, Some(metadata.to_str().unwrap()))
            .await
            .unwrap();
        assert_eq!(tools.nm, PathBuf::from("/target/avr-nm"));
        assert_eq!(tools.objdump, Some(PathBuf::from("/target/avr-objdump")));
        assert_eq!(tools.cppfilt, Some(PathBuf::from("/target/avr-c++filt")));
        std::fs::write(&metadata, br#"{"esp":{"aliases":{"nm":"xtensa-nm"}}}"#).unwrap();
        assert!(
            ToolPaths::resolve(&elf, None, None, Some(metadata.to_str().unwrap()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn resolve_nested_release_metadata_environment() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp
            .path()
            .join(fbuild_paths::FBUILD_DIR_NAME)
            .join(fbuild_paths::BUILD_DIR_NAME)
            .join("uno/release/firmware.elf");
        let metadata = tmp.path().join("build_info.json");
        std::fs::write(
            &metadata,
            br#"{"esp":{"aliases":{"nm":"xtensa-nm"}},"uno":{"aliases":{"nm":"avr-nm"}}}"#,
        )
        .unwrap();
        let tools = ToolPaths::resolve(&elf, None, None, Some(metadata.to_str().unwrap()))
            .await
            .unwrap();
        assert_eq!(tools.nm, PathBuf::from("avr-nm"));
    }

    #[tokio::test]
    async fn resolve_metadata_selects_matching_program_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp.path().join("firmware.elf");
        let metadata = tmp.path().join("build_info.json");
        std::fs::write(
            &metadata,
            serde_json::to_vec(&serde_json::json!({
                "uno": {"prog_path": elf, "aliases": {"nm": "avr-nm"}},
                "esp": {"prog_path": "other.elf", "aliases": {"nm": "xtensa-nm"}}
            }))
            .unwrap(),
        )
        .unwrap();
        let tools = ToolPaths::resolve(&elf, None, None, Some(metadata.to_str().unwrap()))
            .await
            .unwrap();
        assert_eq!(tools.nm, PathBuf::from("avr-nm"));
        std::fs::write(
            &metadata,
            serde_json::to_vec(&serde_json::json!({
                "uno": {"prog_path": elf, "aliases": {"nm": "avr-nm"}},
                "esp": {"prog_path": elf, "aliases": {"nm": "xtensa-nm"}}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(
            ToolPaths::resolve(&elf, None, None, Some(metadata.to_str().unwrap()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn resolve_metadata_matches_symlink_program_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp.path().join("firmware.elf");
        std::fs::write(&elf, b"fixture").unwrap();
        let alias = tmp.path().join("firmware-alias.elf");
        if let Err(error) = fbuild_core::platform::fs::symlink_file(&elf, &alias) {
            if fbuild_core::platform::host::is_windows()
                && error.kind() == std::io::ErrorKind::PermissionDenied
            {
                // Hosts without symlink privileges cannot construct this fixture.
                return;
            }
            panic!("create file symlink: {error}");
        }
        let metadata = tmp.path().join("build_info.json");
        std::fs::write(
            &metadata,
            serde_json::to_vec(&serde_json::json!({
                "uno": {"prog_path": alias, "aliases": {"nm": "avr-nm"}},
                "esp": {"prog_path": "other.elf", "aliases": {"nm": "xtensa-nm"}}
            }))
            .unwrap(),
        )
        .unwrap();
        let tools = ToolPaths::resolve(&elf, None, None, Some(metadata.to_str().unwrap()))
            .await
            .unwrap();
        assert_eq!(tools.nm, PathBuf::from("avr-nm"));
    }

    #[tokio::test]
    async fn resolve_rejects_ambiguous_or_invalid_explicit_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp.path().join("firmware.elf");
        let metadata = tmp.path().join("build_info.json");
        std::fs::write(
            &metadata,
            br#"{"uno":{"aliases":{"nm":"avr-nm"}},"esp":{"aliases":{"nm":"xtensa-nm"}}}"#,
        )
        .unwrap();
        assert!(
            ToolPaths::resolve(&elf, None, None, Some(metadata.to_str().unwrap()))
                .await
                .is_err()
        );
        std::fs::write(&metadata, b"invalid json").unwrap();
        assert!(
            ToolPaths::resolve(&elf, None, None, Some(metadata.to_str().unwrap()))
                .await
                .is_err()
        );
        std::fs::write(&metadata, br#"{"uno":{"aliases":{}}}"#).unwrap();
        assert!(
            ToolPaths::resolve(&elf, None, None, Some(metadata.to_str().unwrap()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn resolve_explicit_nm_selects_its_sibling_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp.path().join("firmware.elf");
        let metadata = tmp.path().join("build_info.json");
        std::fs::write(&metadata, br#"{"uno":{"aliases":{"nm":"wrong-nm","objdump":"wrong-objdump","c++filt":"wrong-c++filt"}}}"#).unwrap();
        let nm = tmp.path().join("avr-nm");
        let objdump = tmp.path().join("avr-objdump");
        let cppfilt = tmp.path().join("avr-c++filt");
        for path in [&nm, &objdump, &cppfilt] {
            std::fs::write(path, b"x").unwrap();
        }
        let tools = ToolPaths::resolve(
            &elf,
            Some(nm.to_str().unwrap()),
            None,
            Some(metadata.to_str().unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(tools.objdump, Some(objdump));
        assert_eq!(tools.cppfilt, Some(cppfilt));
    }
    #[tokio::test]
    async fn resolve_auto_discovery_selects_matching_env_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let elf = tmp.path().join(".pio/build/uno/firmware.elf");
        std::fs::create_dir_all(elf.parent().unwrap()).unwrap();
        std::fs::write(
            tmp.path().join("build_info_esp.json"),
            br#"{"esp":{"aliases":{"nm":"wrong-nm"}}}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("build_info.json"),
            br#"{"esp":{"aliases":{"nm":"wrong-generic-nm"}}}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("build_info_uno.json"),
            br#"{"uno":{"aliases":{"nm":"avr-nm"}}}"#,
        )
        .unwrap();
        let tools = ToolPaths::resolve(&elf, None, None, None).await.unwrap();
        assert_eq!(tools.nm, PathBuf::from("avr-nm"));
    }
}

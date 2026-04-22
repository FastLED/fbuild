//! Teensy build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (teensy40/teensy41)
//! 3. Ensure Teensy-compatible ARM GCC toolchain
//! 4. Ensure Teensy cores
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources (teensy4/*.c, *.cpp)
//! 8. Compile sketch sources
//! 9. Link (with linker script from teensy4/)
//! 10. Convert to hex + report size

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};
use fbuild_packages::library::TeensyFrameworkLibrary;
use serde::Serialize;
use walkdir::{DirEntry, WalkDir};

use crate::build_fingerprint::{
    hash_watch_set_stamps_cached, save_json, stable_hash_json, FastPathInputs,
    PersistedBuildFingerprint, BUILD_FINGERPRINT_VERSION,
};
use crate::compile_database::{CompileDatabase, TargetArchitecture};
use crate::compiler::Compiler as _;
use crate::pipeline;
use crate::zccache::FingerprintWatch;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::teensy_compiler::TeensyCompiler;
use super::teensy_linker::TeensyLinker;

/// Teensy platform build orchestrator.
pub struct TeensyOrchestrator;

#[derive(Debug, Serialize)]
struct TeensyFingerprintMetadata {
    version: u32,
    env_name: String,
    profile: String,
    board_name: String,
    board_mcu: String,
    board_define: String,
    board_core: String,
    board_f_cpu: String,
    board_extra_flags: Option<String>,
    board_ldscript: Option<String>,
    platform: String,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
}

fn profile_label(profile: fbuild_core::BuildProfile) -> &'static str {
    match profile {
        fbuild_core::BuildProfile::Release => "release",
        fbuild_core::BuildProfile::Quick => "quick",
    }
}

fn build_fingerprint_path(build_dir: &Path) -> PathBuf {
    build_dir.join("build_fingerprint.json")
}

fn collect_fast_path_watches(build_dir: &Path, project_dir: &Path) -> Vec<FingerprintWatch> {
    let mut watches = Vec::new();
    if let Some(watch) =
        crate::build_fingerprint::fast_path_watch("project", build_dir, project_dir)
    {
        watches.push(watch);
    }
    let resolved_libs_dir = build_dir.join("libs");
    if let Some(watch) =
        crate::build_fingerprint::fast_path_watch("dep_libs", build_dir, &resolved_libs_dir)
    {
        watches.push(watch);
    }
    watches
}

fn expected_fast_path_artifacts(
    build_dir: &Path,
    project_dir: &Path,
) -> (PathBuf, PathBuf, PathBuf) {
    (
        build_dir.join("firmware.elf"),
        build_dir.join("firmware.hex"),
        CompileDatabase::expected_output_path(build_dir, project_dir),
    )
}

fn resolve_teensy_framework_library_sources(
    framework: &fbuild_packages::library::TeensyCores,
    project_dir: &Path,
    src_dir: &Path,
) -> Vec<PathBuf> {
    let libraries = framework.get_framework_libraries();
    let roots = teensy_include_scan_roots(project_dir, src_dir);
    resolve_teensy_framework_library_sources_from_libraries(&libraries, &roots)
}

fn resolve_teensy_framework_library_sources_from_libraries(
    libraries: &[TeensyFrameworkLibrary],
    roots: &[PathBuf],
) -> Vec<PathBuf> {
    let mut header_to_library = HashMap::new();
    for (idx, library) in libraries.iter().enumerate() {
        let mut headers = HashSet::new();
        for include_dir in &library.include_dirs {
            collect_header_names(include_dir, &mut headers);
        }
        for header in headers {
            header_to_library.entry(header).or_insert(idx);
        }
    }

    let mut pending = HashSet::new();
    for root in roots {
        collect_included_headers(root, &mut pending);
    }

    let mut selected = HashSet::new();
    let mut queue: Vec<String> = pending.iter().cloned().collect();
    while let Some(header) = queue.pop() {
        let Some(&library_idx) = header_to_library.get(&header) else {
            continue;
        };
        if !selected.insert(library_idx) {
            continue;
        }

        let mut transitive_headers = HashSet::new();
        collect_framework_included_headers(&libraries[library_idx].dir, &mut transitive_headers);
        for transitive in transitive_headers {
            if pending.insert(transitive.clone()) {
                queue.push(transitive);
            }
        }
    }

    let mut selected_indices: Vec<_> = selected.into_iter().collect();
    selected_indices.sort_unstable();

    let mut sources = Vec::new();
    for idx in selected_indices {
        tracing::info!(
            "selected Teensy framework library '{}': {} source files",
            libraries[idx].name,
            libraries[idx].source_files.len()
        );
        sources.extend(libraries[idx].source_files.iter().cloned());
    }
    sources.sort();
    sources.dedup();
    sources
}

fn teensy_include_scan_roots(project_dir: &Path, src_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    push_existing_unique(&mut roots, src_dir.to_path_buf());
    push_existing_unique(&mut roots, project_dir.join("src"));
    push_existing_unique(&mut roots, project_dir.join("include"));
    push_existing_unique(&mut roots, project_dir.join("lib"));
    roots
}

fn push_existing_unique(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if !path.exists() {
        return;
    }
    if !roots.iter().any(|existing| existing == &path) {
        roots.push(path);
    }
}

fn collect_header_names(root: &Path, headers: &mut HashSet<String>) {
    if !root.exists() {
        return;
    }

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(should_scan_framework_entry)
        .flatten()
    {
        if !entry.file_type().is_file() || !is_header_file(entry.path()) {
            continue;
        }
        if let Some(name) = entry.path().file_name().and_then(|name| name.to_str()) {
            headers.insert(name.to_string());
        }
    }
}

fn collect_included_headers(root: &Path, headers: &mut HashSet<String>) {
    collect_included_headers_with_filter(root, headers, should_scan_entry);
}

fn collect_framework_included_headers(root: &Path, headers: &mut HashSet<String>) {
    collect_included_headers_with_filter(root, headers, should_scan_framework_entry);
}

fn collect_included_headers_with_filter(
    root: &Path,
    headers: &mut HashSet<String>,
    filter: fn(&DirEntry) -> bool,
) {
    if !root.exists() {
        return;
    }

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(filter)
        .flatten()
    {
        if !entry.file_type().is_file() || !is_source_or_header_file(entry.path()) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for line in content.lines() {
            if let Some(header) = parse_include_header(line) {
                headers.insert(header);
            }
        }
    }
}

fn should_scan_entry(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy().to_lowercase();
    !matches!(
        name.as_str(),
        ".git"
            | ".pio"
            | ".fbuild"
            | ".zap"
            | ".build"
            | "build"
            | "target"
            | ".venv"
            | "venv"
            | "node_modules"
            | "__pycache__"
    )
}

fn should_scan_framework_entry(entry: &DirEntry) -> bool {
    if !should_scan_entry(entry) {
        return false;
    }
    let name = entry.file_name().to_string_lossy().to_lowercase();
    !matches!(
        name.as_str(),
        "examples" | "example" | "extras" | "test" | "tests" | "fontconvert"
    )
}

fn is_source_or_header_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_lowercase();
    matches!(
        ext.as_str(),
        "c" | "cpp" | "cc" | "cxx" | "s" | "ino" | "h" | "hh" | "hpp" | "hxx"
    )
}

fn is_header_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_lowercase();
    matches!(ext.as_str(), "h" | "hh" | "hpp" | "hxx")
}

fn parse_include_header(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let directive = trimmed.strip_prefix('#')?.trim_start();
    let rest = directive.strip_prefix("include")?.trim_start();
    let mut chars = rest.chars();
    let opener = chars.next()?;
    let closer = match opener {
        '<' => '>',
        '"' => '"',
        _ => return None,
    };
    let remainder = &rest[opener.len_utf8()..];
    let end = remainder.find(closer)?;
    let include_path = &remainder[..end];
    Path::new(include_path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
}

impl BuildOrchestrator for TeensyOrchestrator {
    fn platform(&self) -> Platform {
        Platform::Teensy
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();
        let compiler_cache = crate::zccache::find_zccache().map(std::path::Path::to_path_buf);

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params)?;

        // Need board_id for linker script lookup later
        let env_config = ctx.config.get_env_config(&params.env_name)?;
        let board_id = env_config.get("board").ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError("missing 'board' in environment config".into())
        })?;

        // 3. Ensure Teensy-compatible ARM GCC toolchain
        let toolchain = fbuild_packages::toolchain::TeensyArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("Teensy ARM GCC toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        );

        // 4. Ensure Teensy cores
        let framework = fbuild_packages::library::TeensyCores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("Teensy cores at {}", framework_dir.display());

        let core_dir = framework.get_core_dir(&ctx.board.core);
        let build_dir = &ctx.build_dir;
        let fingerprint_path = build_fingerprint_path(build_dir);
        let metadata_hash = stable_hash_json(&TeensyFingerprintMetadata {
            version: BUILD_FINGERPRINT_VERSION,
            env_name: params.env_name.clone(),
            profile: profile_label(params.profile).to_string(),
            board_name: ctx.board.name.clone(),
            board_mcu: ctx.board.mcu.clone(),
            board_define: ctx.board.board.clone(),
            board_core: ctx.board.core.clone(),
            board_f_cpu: ctx.board.f_cpu.clone(),
            board_extra_flags: ctx.board.extra_flags.clone(),
            board_ldscript: ctx.board.ldscript.clone(),
            platform: "teensy".to_string(),
            max_flash: ctx.board.max_flash,
            max_ram: ctx.board.max_ram,
        })?;
        let fingerprint_watches = collect_fast_path_watches(build_dir, &params.project_dir);

        if !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let (fast_elf, fast_hex, fast_compile_db) =
                expected_fast_path_artifacts(build_dir, &params.project_dir);
            let required_artifacts = [fast_elf.clone(), fast_hex.clone(), fast_compile_db.clone()];
            let inputs = FastPathInputs {
                fingerprint_path: &fingerprint_path,
                metadata_hash: &metadata_hash,
                watches: &fingerprint_watches,
                required_artifacts: &required_artifacts,
                extra_artifact_ok: None,
                watch_set_cache: params.watch_set_cache.as_deref(),
                compiler_cache: compiler_cache.as_deref(),
            };
            if let Some(hit) = crate::build_fingerprint::fast_path_check(&inputs)? {
                ctx.build_log.push(
                    "No-op fingerprint matched; reusing existing Teensy artifacts.".to_string(),
                );
                let elapsed = start.elapsed().as_secs_f64();
                return Ok(BuildResult {
                    success: true,
                    firmware_path: Some(fast_hex),
                    elf_path: Some(fast_elf),
                    size_info: hit.size_info,
                    symbol_map: None,
                    build_time_secs: elapsed,
                    message: format!(
                        "Teensy ({}) build for {} reused cached artifacts",
                        ctx.board.mcu, params.env_name
                    ),
                    compile_database_path: Some(fast_compile_db),
                    build_log: ctx.build_log,
                });
            }
        }

        // 5. Scan sources (Teensy: no variants, exclude Blink.cc test sketch)
        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let mut sources =
            scanner.scan_all_filtered(Some(&core_dir), None, ctx.source_filter.as_deref())?;
        sources
            .core_sources
            .retain(|p| p.file_name().map(|f| f != "Blink.cc").unwrap_or(true));

        let framework_library_sources =
            resolve_teensy_framework_library_sources(&framework, &params.project_dir, &ctx.src_dir);
        if !framework_library_sources.is_empty() {
            tracing::info!(
                "Teensy framework library sources added: {}",
                framework_library_sources.len()
            );
            sources.core_sources.extend(framework_library_sources);
        }

        tracing::info!(
            "sources: {} sketch, {} core",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
        );

        // 6. Build include dirs + compiler
        let mcu_config =
            super::mcu_config::get_teensy_config_for_mcu(&ctx.board.mcu.to_lowercase())?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        let mut include_dirs = vec![core_dir.clone()];
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        include_dirs.extend(framework.get_framework_library_include_dirs());
        // Toolchain sysroot includes (ARM CMSIS headers, etc.)
        include_dirs.extend(toolchain.get_include_dirs());

        let compiler = TeensyCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        )
        .with_build_unflags(ctx.build_unflags.clone());

        // 7. Create linker (with linker script from board config)
        let linker_scripts = match ctx.board.ldscript.as_deref() {
            Some(name) => crate::linker::LinkerScripts::single(core_dir.clone(), name),
            None => {
                // Fallback: framework's hardcoded lookup for backward compatibility
                let path = framework.get_linker_script(board_id);
                crate::linker::LinkerScripts {
                    search_dirs: vec![],
                    scripts: vec![path.to_string_lossy().to_string()],
                }
            }
        };
        let linker = TeensyLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_scripts,
            mcu_config,
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        );

        // 8. Build LibraryBuildEnv for project-as-library compilation
        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let ar_path = toolchain.get_ar_path();
        let gcc_ar_path = toolchain.get_gcc_ar_path();
        let c_flags = compiler.c_flags();
        let cpp_flags = compiler.cpp_flags();
        // Use gcc-ar for LTO archives so the linker-plugin index is written.
        let lib_ar_path = pipeline::pick_archiver(&ar_path, &gcc_ar_path, &c_flags, &cpp_flags);
        let lib_env = pipeline::LibraryBuildEnv {
            gcc_path: &gcc_path,
            gxx_path: &gxx_path,
            ar_path: lib_ar_path,
            c_flags: &c_flags,
            cpp_flags: &cpp_flags,
            include_dirs: &include_dirs,
            verbose: params.verbose,
            jobs: crate::parallel::effective_jobs(params.jobs),
            compiler_cache: None,
        };

        // 9. Run shared sequential build pipeline
        let build_result = pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &[],
            Some(&lib_env),
            TargetArchitecture::Arm,
            "Teensy",
            start,
        )?;

        if build_result.success
            && !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let persisted_fingerprint = PersistedBuildFingerprint {
                version: BUILD_FINGERPRINT_VERSION,
                metadata_hash: metadata_hash.clone(),
                file_set_hash: match hash_watch_set_stamps_cached(
                    &fingerprint_watches,
                    params.watch_set_cache.as_deref(),
                ) {
                    Ok(hash) => Some(hash),
                    Err(e) => {
                        tracing::warn!("failed to hash watched inputs for fingerprint save: {}", e);
                        None
                    }
                },
                size_info: build_result.size_info.clone(),
            };
            if let Err(e) = save_json(&fingerprint_path, &persisted_fingerprint) {
                tracing::warn!("failed to write build fingerprint: {}", e);
            }
            if let Some(ref zcc) = compiler_cache {
                for watch in &fingerprint_watches {
                    if let Err(e) = crate::zccache::mark_fingerprint_success(zcc, watch) {
                        tracing::warn!(
                            "failed to mark zccache fingerprint success for {}: {}",
                            watch.root.display(),
                            e
                        );
                    }
                }
            }
        }

        Ok(build_result)
    }
}

/// Create a Teensy orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(TeensyOrchestrator)
}

/// Check if a project is configured for Teensy by reading its platformio.ini.
pub fn is_teensy_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::Teensy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_teensy_orchestrator_platform() {
        let orch = TeensyOrchestrator;
        assert_eq!(orch.platform(), Platform::Teensy);
    }

    #[test]
    fn test_is_teensy_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:teensy41]\nplatform = teensy\nboard = teensy41\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_teensy_project(tmp.path(), "teensy41"));
        assert!(!is_teensy_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_is_not_teensy_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_teensy_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_collect_fast_path_watches_skips_missing_dep_libs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        let watches = collect_fast_path_watches(&build_dir, &project_dir);
        assert_eq!(watches.len(), 1);
        assert_eq!(watches[0].root, project_dir);
    }

    #[test]
    fn test_expected_fast_path_artifacts_follow_compile_db_location() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let app_project = tmp.path().join("app");
        let lib_project = tmp.path().join("libproj");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&app_project).unwrap();
        std::fs::create_dir_all(&lib_project).unwrap();
        std::fs::write(lib_project.join("library.json"), r#"{"name":"libproj"}"#).unwrap();

        let (_, _, app_compile_db) = expected_fast_path_artifacts(&build_dir, &app_project);
        let (_, _, lib_compile_db) = expected_fast_path_artifacts(&build_dir, &lib_project);

        assert_eq!(app_compile_db, app_project.join("compile_commands.json"));
        assert_eq!(lib_compile_db, build_dir.join("compile_commands.json"));
    }

    #[test]
    fn test_parse_include_header_extracts_basename() {
        assert_eq!(
            parse_include_header("#include <SPI.h>"),
            Some("SPI.h".to_string())
        );
        assert_eq!(
            parse_include_header("  # include \"utility/foo.hpp\""),
            Some("foo.hpp".to_string())
        );
        assert_eq!(parse_include_header("int x = 1;"), None);
    }

    #[test]
    fn test_resolve_teensy_framework_libraries_from_project_includes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(
            project_src.join("main.cpp"),
            "#include <SPI.h>\n#include <OctoWS2811.h>\n",
        )
        .unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let octo_dir = tmp
            .path()
            .join("framework")
            .join("libraries")
            .join("OctoWS2811");
        std::fs::create_dir_all(&octo_dir).unwrap();
        std::fs::write(octo_dir.join("OctoWS2811.h"), "").unwrap();
        std::fs::write(octo_dir.join("OctoWS2811.cpp"), "").unwrap();
        std::fs::write(octo_dir.join("OctoWS2811_imxrt.cpp"), "").unwrap();

        let libraries = vec![
            TeensyFrameworkLibrary {
                name: "OctoWS2811".to_string(),
                dir: octo_dir.clone(),
                include_dirs: vec![octo_dir.clone()],
                source_files: vec![
                    octo_dir.join("OctoWS2811.cpp"),
                    octo_dir.join("OctoWS2811_imxrt.cpp"),
                ],
            },
            TeensyFrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let mut sources = resolve_teensy_framework_library_sources_from_libraries(
            &libraries,
            std::slice::from_ref(&project_src),
        );
        sources.sort();

        assert_eq!(
            sources,
            vec![
                octo_dir.join("OctoWS2811.cpp"),
                octo_dir.join("OctoWS2811_imxrt.cpp"),
                spi_dir.join("SPI.cpp"),
            ]
        );
    }

    #[test]
    fn test_resolve_teensy_framework_libraries_follows_transitive_includes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(project_src.join("main.cpp"), "#include <NeedsSpi.h>\n").unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let wrapper_dir = tmp
            .path()
            .join("framework")
            .join("libraries")
            .join("NeedsSpi");
        std::fs::create_dir_all(&wrapper_dir).unwrap();
        std::fs::write(wrapper_dir.join("NeedsSpi.h"), "#include <SPI.h>\n").unwrap();
        std::fs::write(wrapper_dir.join("NeedsSpi.cpp"), "").unwrap();

        let libraries = vec![
            TeensyFrameworkLibrary {
                name: "NeedsSpi".to_string(),
                dir: wrapper_dir.clone(),
                include_dirs: vec![wrapper_dir.clone()],
                source_files: vec![wrapper_dir.join("NeedsSpi.cpp")],
            },
            TeensyFrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let mut sources = resolve_teensy_framework_library_sources_from_libraries(
            &libraries,
            std::slice::from_ref(&project_src),
        );
        sources.sort();

        assert_eq!(
            sources,
            vec![wrapper_dir.join("NeedsSpi.cpp"), spi_dir.join("SPI.cpp")]
        );
    }
}

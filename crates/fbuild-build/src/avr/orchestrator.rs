//! AVR build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config
//! 3. Ensure avr-gcc toolchain
//! 4. Ensure Arduino AVR core
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile all sources
//! 8. Link into firmware.elf
//! 9. Convert to firmware.hex
//! 10. Report size

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compiler::{Compiler, CompilerBase};
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::avr_compiler::AvrCompiler;
use super::avr_linker::AvrLinker;

/// AVR platform build orchestrator.
pub struct AvrOrchestrator;

impl BuildOrchestrator for AvrOrchestrator {
    fn platform(&self) -> Platform {
        Platform::AtmelAvr
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1. Parse platformio.ini
        let ini_path = params.project_dir.join("platformio.ini");
        let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
        let env_config = config.get_env_config(&params.env_name)?;

        // 2. Load board config
        let board_id = env_config.get("board").ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError("missing 'board' in environment config".into())
        })?;
        let overrides = config.get_board_overrides(&params.env_name)?;
        let board = fbuild_config::BoardConfig::from_board_id(board_id, &overrides)?;

        let mut build_log = crate::build_output::create_build_log(params.log_sender.clone());
        crate::build_output::log_build_banner(&mut build_log, &params.env_name);
        crate::build_output::log_board_info(
            &mut build_log,
            &board.name,
            &board.mcu,
            &board.f_cpu,
            board.max_flash,
            board.max_ram,
        );

        // 3. Ensure toolchain
        let toolchain = fbuild_packages::toolchain::AvrToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("avr-gcc toolchain at {}", toolchain_dir.display());

        // Toolchain version
        use fbuild_packages::Toolchain as _;
        if let Ok(ver_out) = fbuild_core::subprocess::run_command(
            &[
                toolchain.get_gcc_path().to_string_lossy().as_ref(),
                "-dumpversion",
            ],
            None,
            None,
            None,
        ) {
            let version = ver_out.stdout.trim().to_string();
            if !version.is_empty() {
                crate::build_output::log_toolchain_version(&mut build_log, "avr-gcc", &version);
            }
        }

        // 4. Ensure Arduino core (select framework based on board's core name)
        let (framework_dir, core_dir, variant_dir) =
            ensure_avr_framework(&params.project_dir, &board.core, &board.variant)?;

        // 5. Setup build directories
        let cache = fbuild_packages::Cache::new(&params.project_dir);
        if params.clean {
            cache.clean_build(&params.env_name, params.profile)?;
        }
        cache.ensure_build_directories(&params.env_name, params.profile)?;

        let build_dir = cache.get_build_dir(&params.env_name, params.profile);
        let core_build_dir = cache.get_core_build_dir(&params.env_name, params.profile);
        let src_build_dir = cache.get_src_build_dir(&params.env_name, params.profile);

        // 6. Scan sources
        let src_dir = params.project_dir.join(
            config
                .get_src_dir(&params.env_name)?
                .unwrap_or_else(|| "src".to_string()),
        );
        // Fall back to project root if src/ doesn't exist (Arduino IDE convention)
        let src_dir = if src_dir.exists() {
            src_dir
        } else {
            params.project_dir.clone()
        };

        let scanner = SourceScanner::new(&src_dir, &src_build_dir);
        let sources = scanner.scan_all(Some(&core_dir), Some(&variant_dir))?;

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 7. Compile
        let defines = board.get_defines();
        let mut include_dirs = board.get_include_paths(&framework_dir);
        include_dirs.push(src_dir.clone());

        // PlatformIO automatically includes the project's include/ directory
        let include_dir = params.project_dir.join("include");
        if include_dir.is_dir() {
            include_dirs.push(include_dir);
        }

        // PlatformIO automatically discovers libraries placed in the project's lib/ directory.
        // Each subdirectory is treated as a library — add its root (and src/ if present).
        let local_lib_dir = params.project_dir.join("lib");
        if local_lib_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&local_lib_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let lib_src = path.join("src");
                        if lib_src.is_dir() {
                            include_dirs.push(lib_src);
                        }
                        // Always add the root too (some libraries have headers at top level)
                        include_dirs.push(path);
                    }
                }
            }
        }

        // Add user build flags to defines
        let user_flags = config.get_build_flags(&params.env_name)?;
        crate::warn_debug_build_flags(&user_flags);

        let mcu_config = super::mcu_config::get_avr_config()?;

        let compiler = AvrCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &board.mcu,
            &board.f_cpu,
            defines,
            include_dirs,
            mcu_config.clone(),
            params.verbose,
        );

        // Build flags needed for compile_commands.json
        let src_flags = config.get_build_src_flags(&params.env_name)?;
        let all_src_flags: Vec<String> =
            user_flags.iter().chain(src_flags.iter()).cloned().collect();

        // compiledb_only: generate compile_commands.json without compiling
        if params.compiledb_only {
            let core_and_variant: Vec<std::path::PathBuf> = sources
                .core_sources
                .iter()
                .chain(sources.variant_sources.iter())
                .cloned()
                .collect();
            let mut compile_db = crate::compile_database::CompileDatabase::new();
            compile_db.extend(crate::compile_database::generate_entries(
                compiler.gcc_path(),
                compiler.gxx_path(),
                &compiler.c_flags(),
                &compiler.cpp_flags(),
                &[],
                &user_flags,
                &core_and_variant,
                &core_build_dir,
                &params.project_dir,
            ));
            compile_db.extend(crate::compile_database::generate_entries(
                compiler.gcc_path(),
                compiler.gxx_path(),
                &compiler.c_flags(),
                &compiler.cpp_flags(),
                &[],
                &all_src_flags,
                &sources.sketch_sources,
                &src_build_dir,
                &params.project_dir,
            ));
            let compile_db =
                compile_db.translate_for_clang(crate::compile_database::TargetArchitecture::Avr);
            let compile_database_path = if compile_db.has_entries() {
                Some(compile_db.write_and_copy(&build_dir, &params.project_dir)?)
            } else {
                None
            };
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult {
                success: true,
                hex_path: None,
                elf_path: None,
                size_info: None,
                build_time_secs: elapsed,
                message: format!("compile_commands.json generated for {}", params.env_name),
                compile_database_path,
                build_log,
            });
        }

        // Compile core sources
        let mut core_objects = Vec::new();
        for source in &sources.core_sources {
            let obj = CompilerBase::object_path(source, &core_build_dir);
            if CompilerBase::needs_rebuild(source, &obj) {
                crate::build_output::log_compiling(&mut build_log, &obj);
                let result = compiler.compile(source, &obj, &user_flags)?;
                if !result.success {
                    return Err(fbuild_core::FbuildError::BuildFailed(format!(
                        "compilation failed for {}:\n{}",
                        source.display(),
                        result.stderr
                    )));
                }
                crate::build_output::collect_warnings(&result.stderr, &mut build_log);
            }
            core_objects.push(obj);
        }

        // Compile variant sources
        for source in &sources.variant_sources {
            let obj = CompilerBase::object_path(source, &core_build_dir);
            if CompilerBase::needs_rebuild(source, &obj) {
                crate::build_output::log_compiling(&mut build_log, &obj);
                let result = compiler.compile(source, &obj, &user_flags)?;
                if !result.success {
                    return Err(fbuild_core::FbuildError::BuildFailed(format!(
                        "compilation failed for {}:\n{}",
                        source.display(),
                        result.stderr
                    )));
                }
                crate::build_output::collect_warnings(&result.stderr, &mut build_log);
            }
            core_objects.push(obj);
        }

        // Compile sketch sources (with src flags too)
        let mut sketch_objects = Vec::new();
        for source in &sources.sketch_sources {
            let obj = CompilerBase::object_path(source, &src_build_dir);
            if CompilerBase::needs_rebuild(source, &obj) {
                crate::build_output::log_compiling(&mut build_log, &obj);
                let result = compiler.compile(source, &obj, &all_src_flags)?;
                if !result.success {
                    return Err(fbuild_core::FbuildError::BuildFailed(format!(
                        "compilation failed for {}:\n{}",
                        source.display(),
                        result.stderr
                    )));
                }
                crate::build_output::collect_warnings(&result.stderr, &mut build_log);
            }
            sketch_objects.push(obj);
        }

        // 7.5. Compile local libraries from the project's lib/ directory.
        let mut library_objects = Vec::new();
        let local_lib_dir = params.project_dir.join("lib");
        if local_lib_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&local_lib_dir) {
                for entry in entries.flatten() {
                    let lib_path = entry.path();
                    if !lib_path.is_dir() {
                        continue;
                    }
                    let lib_name = lib_path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    let lib_info = fbuild_packages::library::library_info::InstalledLibrary::new(
                        &lib_path, &lib_name,
                    );
                    let lib_sources = lib_info.get_source_files();
                    if lib_sources.is_empty() {
                        continue;
                    }

                    let lib_build_dir = build_dir.join("lib").join(&lib_name);
                    std::fs::create_dir_all(&lib_build_dir)?;
                    tracing::info!(
                        "compiling local library '{}': {} source files",
                        lib_name,
                        lib_sources.len()
                    );

                    for source in &lib_sources {
                        let obj = CompilerBase::object_path(source, &lib_build_dir);
                        if CompilerBase::needs_rebuild(source, &obj) {
                            crate::build_output::log_compiling(&mut build_log, &obj);
                            let result = compiler.compile(source, &obj, &all_src_flags)?;
                            if !result.success {
                                return Err(fbuild_core::FbuildError::BuildFailed(format!(
                                    "local library '{}' compilation failed for {}:\n{}",
                                    lib_name,
                                    source.display(),
                                    result.stderr
                                )));
                            }
                            crate::build_output::collect_warnings(&result.stderr, &mut build_log);
                        }
                        library_objects.push(obj);
                    }
                }
            }
        }

        // 7.6. Generate compile_commands.json
        let mut compile_db = crate::compile_database::CompileDatabase::new();
        // Core + variant sources use user_flags
        let core_and_variant: Vec<std::path::PathBuf> = sources
            .core_sources
            .iter()
            .chain(sources.variant_sources.iter())
            .cloned()
            .collect();
        compile_db.extend(crate::compile_database::generate_entries(
            compiler.gcc_path(),
            compiler.gxx_path(),
            &compiler.c_flags(),
            &compiler.cpp_flags(),
            &[], // AVR: include flags already in c/cpp_flags
            &user_flags,
            &core_and_variant,
            &core_build_dir,
            &params.project_dir,
        ));
        // Sketch sources use all_src_flags
        compile_db.extend(crate::compile_database::generate_entries(
            compiler.gcc_path(),
            compiler.gxx_path(),
            &compiler.c_flags(),
            &compiler.cpp_flags(),
            &[],
            &all_src_flags,
            &sources.sketch_sources,
            &src_build_dir,
            &params.project_dir,
        ));
        let compile_db =
            compile_db.translate_for_clang(crate::compile_database::TargetArchitecture::Avr);
        let compile_database_path = if compile_db.has_entries() {
            Some(compile_db.write_and_copy(&build_dir, &params.project_dir)?)
        } else {
            None
        };

        // 8-9. Link + convert
        crate::build_output::log_linking(&mut build_log, "Linking firmware.elf");
        let linker = AvrLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            &board.mcu,
            mcu_config,
            board.max_flash,
            board.max_ram,
            params.verbose,
        );

        // Include library objects alongside core objects for linking
        let mut all_core_objects = core_objects;
        all_core_objects.extend(library_objects);

        let link_result = crate::linker::Linker::link_all(
            &linker,
            &sketch_objects,
            &all_core_objects,
            &build_dir,
        )?;

        if link_result.hex_path.is_some() {
            crate::build_output::log_linking(&mut build_log, "Building firmware.hex");
        } else if link_result.bin_path.is_some() {
            crate::build_output::log_linking(&mut build_log, "Building firmware.bin");
        }

        // 10. Size reporting with totals + percentages
        if let Some(ref size) = link_result.size_info {
            tracing::info!(
                "size: text={} data={} bss={} | flash={}/{} ({:.1}%) ram={}/{} ({:.1}%)",
                size.text,
                size.data,
                size.bss,
                size.total_flash,
                size.max_flash.unwrap_or(0),
                size.flash_percent().unwrap_or(0.0),
                size.total_ram,
                size.max_ram.unwrap_or(0),
                size.ram_percent().unwrap_or(0.0),
            );
            crate::build_output::log_size_report(&mut build_log, size);
        }

        // Artifact listing
        if let Some(ref elf) = link_result.elf_path {
            crate::build_output::log_artifact(&mut build_log, elf);
        }
        let firmware_path = link_result
            .hex_path
            .as_ref()
            .or(link_result.bin_path.as_ref());
        if let Some(fw) = firmware_path {
            crate::build_output::log_artifact(&mut build_log, fw);
        }

        let elapsed = start.elapsed().as_secs_f64();
        tracing::info!("build completed in {:.1}s", elapsed);

        Ok(BuildResult {
            success: true,
            hex_path: link_result.hex_path,
            elf_path: link_result.elf_path,
            size_info: link_result.size_info,
            build_time_secs: elapsed,
            message: format!("AVR build for {} completed", params.env_name),
            compile_database_path,
            build_log,
        })
    }
}

/// Create an AVR orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(AvrOrchestrator)
}

/// Select and install the correct AVR Arduino framework based on the board's core name.
///
/// Uses the data-driven `avr_frameworks.json` registry to resolve the correct
/// framework package (GitHub URL, version) for any board core.
/// Returns (framework_root, core_dir, variant_dir).
fn ensure_avr_framework(
    project_dir: &Path,
    core_name: &str,
    variant_name: &str,
) -> fbuild_core::Result<(PathBuf, PathBuf, PathBuf)> {
    use fbuild_packages::Package;

    let framework = fbuild_packages::library::AvrFramework::for_core(core_name, project_dir)?;
    let framework_dir = framework.ensure_installed()?;
    tracing::info!(
        "AVR framework for core '{}' at {}",
        core_name,
        framework_dir.display()
    );
    let core_dir = framework.get_core_dir(core_name);
    let variant_dir = framework.get_variant_dir(variant_name);
    Ok((framework_dir, core_dir, variant_dir))
}

/// Check if a project is configured for AVR by reading its platformio.ini.
pub fn is_avr_project(project_dir: &Path, env_name: &str) -> bool {
    let ini_path = project_dir.join("platformio.ini");
    if let Ok(config) = fbuild_config::PlatformIOConfig::from_path(&ini_path) {
        if let Ok(env) = config.get_env_config(env_name) {
            if let Some(platform) = env.get("platform") {
                return fbuild_core::Platform::AtmelAvr.matches_str(platform);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_avr_orchestrator_platform() {
        let orch = AvrOrchestrator;
        assert_eq!(orch.platform(), Platform::AtmelAvr);
    }

    #[test]
    fn test_is_avr_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_avr_project(tmp.path(), "uno"));
        assert!(!is_avr_project(tmp.path(), "esp32"));
    }

    #[test]
    fn test_is_not_avr_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp32]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_avr_project(tmp.path(), "esp32"));
    }
}

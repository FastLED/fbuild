//! CH32V build orchestrator â€” wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (genericCH32V003F4P6, etc.)
//! 3. Ensure RISC-V GCC toolchain
//! 4. Ensure OpenWCH CH32V cores
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources
//! 8. Compile sketch sources
//! 9. Link (with linker script)
//! 10. Convert to binary + report size

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::ch32v_compiler::Ch32vCompiler;
use super::ch32v_linker::Ch32vLinker;

/// CH32V platform build orchestrator.
pub struct Ch32vOrchestrator;

#[async_trait::async_trait]
impl BuildOrchestrator for Ch32vOrchestrator {
    fn platform(&self) -> Platform {
        Platform::Ch32v
    }

    async fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params).await?;
        let framework_name = ctx
            .config
            .get_env_config(&params.env_name)
            .ok()
            .and_then(|env| env.get("framework"))
            .map(String::as_str);
        validate_ch32v_framework(framework_name)?;

        // 3. Ensure RISC-V GCC toolchain
        let toolchain = fbuild_packages::toolchain::RiscvToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain).await?;
        tracing::info!("riscv-gcc toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "riscv-none-elf-gcc",
            &mut ctx.build_log,
        )
        .await;

        // 4. Ensure OpenWCH CH32V cores
        // Honor `platform_packages` override (FastLED/fbuild#664, #681).
        let __ovr = ctx
            .config
            .get_env_config(&params.env_name)
            .ok()
            .and_then(|env| {
                crate::package_override::resolve_override(env, "framework-arduino-ch32v")
            });
        let framework = match __ovr {
            Some(o) => fbuild_packages::library::Ch32vCores::with_override(&params.project_dir, o),
            None => fbuild_packages::library::Ch32vCores::new(&params.project_dir),
        };
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework).await?;
        tracing::info!("CH32V cores at {}", framework_dir.display());

        // 5. Resolve series/variant selection used by both scanning and compile flags.
        // Derive the series from the MCU name (e.g. "ch32v003f4p6" -> "ch32v003").
        // CH32V MCU names follow: ch32{letter}{3-digit series}{package suffix}
        // Skip the leading "ch32" prefix, then take the letter + digits.
        let mcu_lower = ctx.board.mcu.to_lowercase();
        let series = if let Some(after_prefix) = mcu_lower.strip_prefix("ch32") {
            // e.g. "v003f4p6"
            let series_end = after_prefix
                .char_indices()
                .skip(1) // skip the letter (v, x, l)
                .find(|(_, c)| !c.is_ascii_digit())
                .map(|(i, _)| i)
                .unwrap_or(after_prefix.len());
            format!("ch32{}", &after_prefix[..series_end])
        } else {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "CH32V board MCU must start with `ch32`; got `{}`",
                ctx.board.mcu
            )));
        };
        let system_series = series_to_system_dir(&series);

        // 6. Scan sources
        let core_dir = framework.get_core_dir(&ctx.board.core);
        let variant_dir = resolve_variant_dir(&framework_dir, &ctx.board.variant, &system_series);

        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let sources = scanner.scan_all_filtered(
            Some(&core_dir),
            Some(&variant_dir),
            ctx.source_filter.as_deref(),
        )?;

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 7. Build include dirs + compiler
        let mut mcu_config = super::mcu_config::get_ch32v_config_for_mcu(&series)?;
        super::mcu_config::apply_board_isa(
            &mut mcu_config,
            ctx.board.march.as_deref(),
            ctx.board.mabi.as_deref(),
        );
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        defines.insert(system_series.clone(), "1".to_string());
        let (sysclk_name, sysclk_value) = sysclk_define(
            &series,
            &ctx.board.f_cpu,
            ctx.board.clock_source.as_deref().unwrap_or("hsi+pll"),
        )?;
        defines.insert(sysclk_name, sysclk_value);
        // CH32V cores use `#include VARIANT_H` â€” define it from the variant dir
        if let Some(vh) = resolve_variant_h(&variant_dir, ctx.board.variant_h.as_deref()) {
            defines.insert("VARIANT_H".to_string(), format!("\\\"{}\\\"", vh));
        }
        // Use resolved core_dir/variant_dir directly â€” board.get_include_paths()
        // uses the raw board core name which may differ from the actual directory
        // (e.g. OpenWCH core dir is "arduino", not "openwch").
        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone()];
        // Core subdirectories (ch32/, ch32/lib/) contain essential headers
        discover_header_subdirs(&core_dir, &mut include_dirs);
        // System HAL headers (Peripheral/inc, Core, USER)
        discover_system_includes(&framework_dir, &series, &mut include_dirs);
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes
        include_dirs.extend(toolchain.get_include_dirs());

        // xPack RISC-V GCC 14.x can't resolve its own multilib C++ include path
        // on Windows; add it explicitly via -isystem (not -I, which doesn't work
        // for GCC's C++ wrapper headers that use #include_next).
        let march = mcu_config
            .compiler_flags
            .common
            .iter()
            .find_map(|f| f.strip_prefix("-march="))
            .unwrap_or("rv32ec");
        let mabi = mcu_config
            .compiler_flags
            .common
            .iter()
            .find_map(|f| f.strip_prefix("-mabi="))
            .unwrap_or("ilp32e");
        let isystem_flags: Vec<String> = toolchain
            .get_cxx_system_includes(march, mabi)
            .into_iter()
            .flat_map(|p| vec!["-isystem".to_string(), p.to_string_lossy().to_string()])
            .collect();

        let compiler = Ch32vCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
            isystem_flags,
        )
        .with_build_unflags(ctx.build_unflags.clone())
        // Scope third-party `-Wno-*` suppressions to the OpenWCH core/variant
        // install only. See FastLED/fbuild#382.
        .with_framework_root(framework_dir.clone());

        // 7. Create linker (resolve linker script from system dir)
        // CH32V linker scripts are in system/<SERIES>/SRC/Ld/, not in variants/
        let linker_script_path = framework_dir
            .join("system")
            .join(&system_series)
            .join("SRC")
            .join("Ld")
            .join("Link.ld");
        let mut memory_defsyms = Vec::new();
        if let (Some(flash), Some(ram)) = (ctx.board.max_flash, ctx.board.max_ram) {
            memory_defsyms.push(format!("-Wl,--defsym=__FLASH_SIZE={flash}"));
            memory_defsyms.push(format!("-Wl,--defsym=__RAM_SIZE={ram}"));
        }
        let linker = Ch32vLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script_path,
            mcu_config,
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        )
        .with_memory_defsyms(memory_defsyms);

        // 8. Build LibraryBuildEnv for project-as-library compilation
        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let ar_path = toolchain.get_ar_path();
        let gcc_ar_path = toolchain.get_gcc_ar_path();
        let c_flags = crate::compiler::Compiler::c_flags(&compiler);
        let cpp_flags = crate::compiler::Compiler::cpp_flags(&compiler);
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
        pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &[],
            Some(&lib_env),
            TargetArchitecture::Riscv32,
            "CH32V",
            start,
        )
        .await
    }
}

/// Create a CH32V orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Ch32vOrchestrator)
}

/// Only the Arduino framework is implemented for CH32V.
fn validate_ch32v_framework(framework: Option<&str>) -> fbuild_core::Result<()> {
    match framework.map(str::trim) {
        None | Some("") | Some("arduino") => Ok(()),
        Some(other) => Err(fbuild_core::FbuildError::ConfigError(format!(
            "ch32v: framework = {other} is not supported yet; only `framework = arduino` builds today (FastLED/fbuild#1108)"
        ))),
    }
}

fn sysclk_define(
    series: &str,
    f_cpu: &str,
    clock_source: &str,
) -> fbuild_core::Result<(String, String)> {
    let hz = f_cpu.trim_end_matches('L').parse::<u64>().map_err(|_| {
        fbuild_core::FbuildError::ConfigError(format!(
            "ch32v: invalid f_cpu `{f_cpu}` for {series}"
        ))
    })?;
    let mhz = hz / 1_000_000;
    if mhz == 0 || hz % 1_000_000 != 0 {
        return Err(fbuild_core::FbuildError::ConfigError(format!(
            "ch32v: unsupported f_cpu `{f_cpu}` for {series}; expected a whole MHz value"
        )));
    }
    let supported: &[u64] = match series {
        "ch32v003" | "ch32v006" => &[24, 48],
        "ch32v103" => &[48, 56, 72],
        "ch32v203" | "ch32v208" | "ch32v303" | "ch32v307" => &[48, 56, 72, 96, 120, 144],
        "ch32x035" => &[8, 12, 16, 24, 48],
        "ch32l103" => &[48, 72, 96],
        _ => &[],
    };
    if !supported.contains(&mhz) {
        return Err(fbuild_core::FbuildError::ConfigError(format!(
            "ch32v: unsupported f_cpu `{f_cpu}` for {series}; supported values: {supported:?} MHz"
        )));
    }
    let source = match clock_source.trim().to_ascii_lowercase().as_str() {
        "hsi" | "hsi+pll" => "HSI",
        "hse" | "hse+pll" => "HSE",
        other => {
            return Err(fbuild_core::FbuildError::ConfigError(format!(
                "ch32v: unsupported clock_source `{other}` for {series}; expected hsi, hsi+pll, hse, or hse+pll"
            )));
        }
    };
    let unit = if matches!(series, "ch32v003" | "ch32v006") && source == "HSI" {
        "MHZ"
    } else {
        "MHz"
    };
    Ok((format!("SYSCLK_FREQ_{mhz}{unit}_{source}"), hz.to_string()))
}

/// Recursively add subdirectories that contain .h files as include paths.
fn discover_header_subdirs(dir: &Path, include_dirs: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                include_dirs.push(path.clone());
                discover_header_subdirs(&path, include_dirs);
            }
        }
    }
}

/// Add system HAL include directories for a CH32V series.
///
/// The OpenWCH core has a `system/<SERIES>/` directory with vendor HAL headers
/// in subdirectories like `SRC/Peripheral/inc/`, `SRC/Core/`, and `USER/`.
fn discover_system_includes(framework_dir: &Path, series: &str, include_dirs: &mut Vec<PathBuf>) {
    // Map series name (e.g. "ch32v003") to system dir name (e.g. "CH32V00x")
    let system_dir_name = series_to_system_dir(series);
    let system_dir = framework_dir.join("system").join(&system_dir_name);
    if !system_dir.exists() {
        tracing::debug!(
            "CH32V system dir not found: {} (series={}, mapped={})",
            system_dir.display(),
            series,
            system_dir_name
        );
        return;
    }
    // Add USER/ and all subdirectories under SRC/ (the OpenWCH core's
    // ch32yyxx_*.c templates #include series-specific .c files from Peripheral/src/)
    let user_dir = system_dir.join("USER");
    if user_dir.is_dir() {
        include_dirs.push(user_dir);
    }
    let src_dir = system_dir.join("SRC");
    if src_dir.is_dir() {
        discover_header_subdirs(&src_dir, include_dirs);
    }
}

/// Find the variant_*.h file in a variant directory.
/// Returns the filename (e.g. "variant_CH32V003F4.h") if found.
fn find_variant_h(variant_dir: &Path) -> Option<String> {
    if let Ok(entries) = std::fs::read_dir(variant_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("variant_") && name.ends_with(".h") {
                return Some(name);
            }
        }
    }
    None
}

fn resolve_variant_h(variant_dir: &Path, preferred: Option<&str>) -> Option<String> {
    preferred
        .filter(|name| variant_dir.join(name).exists())
        .map(ToOwned::to_owned)
        .or_else(|| find_variant_h(variant_dir))
}

fn resolve_variant_dir(
    framework_dir: &Path,
    requested_variant: &str,
    system_series: &str,
) -> PathBuf {
    let requested = framework_dir.join("variants").join(requested_variant);
    if requested.is_dir() {
        return requested;
    }

    let family_dir = framework_dir.join("variants").join(system_series);
    if !family_dir.is_dir() {
        return requested;
    }

    let requested_leaf = Path::new(requested_variant)
        .file_name()
        .and_then(|name| name.to_str());
    if let Some(leaf) = requested_leaf {
        let candidate = family_dir.join(leaf);
        if candidate.is_dir() {
            return candidate;
        }
    }

    let mut variant_dirs = std::fs::read_dir(&family_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    variant_dirs.sort();
    variant_dirs.into_iter().next().unwrap_or(requested)
}

/// Map a series name to the system directory name in the OpenWCH core.
/// e.g. "ch32v003" -> "CH32V00x", "ch32l103" -> "CH32L10x", "ch32x035" -> "CH32X035"
fn series_to_system_dir(series: &str) -> String {
    if matches!(
        series,
        "ch32v002" | "ch32v004" | "ch32v005" | "ch32v006" | "ch32v007" | "ch32m007"
    ) {
        return "CH32VM00X".to_string();
    }
    let upper = series.to_uppercase();
    if upper.len() >= 7 {
        // CH32V/CH32L series use the "replace last digit with x" family
        // directory pattern (CH32V00x, CH32V10x, CH32L10x, etc.).
        // CH32X035 uses its exact uppercase directory name.
        if upper.starts_with("CH32V") || upper.starts_with("CH32L") {
            format!("{}x", &upper[..upper.len() - 1])
        } else {
            upper
        }
    } else {
        upper
    }
}

/// Check if a project is configured for CH32V by reading its platformio.ini.
pub fn is_ch32v_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::Ch32v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::TempDir::new_in(fbuild_paths::temp_subdir("fbuild-ch32v-tests")).unwrap()
    }

    #[test]
    fn test_ch32v_orchestrator_platform() {
        let orch = Ch32vOrchestrator;
        assert_eq!(orch.platform(), Platform::Ch32v);
    }

    #[test]
    fn test_is_ch32v_project() {
        let tmp = tempdir();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:ch32v003]\nplatform = ch32v\nboard = genericCH32V003F4P6\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_ch32v_project(tmp.path(), "ch32v003"));
        assert!(!is_ch32v_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_is_not_ch32v_project() {
        let tmp = tempdir();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_ch32v_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_validate_ch32v_framework() {
        assert!(validate_ch32v_framework(None).is_ok());
        assert!(validate_ch32v_framework(Some("arduino")).is_ok());
        assert!(validate_ch32v_framework(Some(" arduino ")).is_ok());
        let error = validate_ch32v_framework(Some("noneos-sdk")).unwrap_err();
        assert!(error.to_string().contains("1108"));
    }

    #[test]
    fn test_sysclk_define_uses_series_spelling_and_clock_source() {
        assert_eq!(
            sysclk_define("ch32v203", "144000000L", "hsi+pll").unwrap(),
            (
                "SYSCLK_FREQ_144MHz_HSI".to_string(),
                "144000000".to_string()
            )
        );
        assert_eq!(
            sysclk_define("ch32v003", "48000000L", "hsi+pll").unwrap(),
            ("SYSCLK_FREQ_48MHZ_HSI".to_string(), "48000000".to_string())
        );
    }

    #[test]
    fn test_sysclk_define_rejects_unsupported_frequency() {
        let error = sysclk_define("ch32v203", "8000000L", "hsi").unwrap_err();
        assert!(error.to_string().contains("supported values"));
    }

    #[test]
    fn test_series_to_system_dir() {
        // CH32V series: last digit replaced with 'x'
        assert_eq!(series_to_system_dir("ch32v003"), "CH32V00x");
        assert_eq!(series_to_system_dir("ch32v006"), "CH32VM00X");
        assert_eq!(series_to_system_dir("ch32v103"), "CH32V10x");
        assert_eq!(series_to_system_dir("ch32v203"), "CH32V20x");
        assert_eq!(series_to_system_dir("ch32v303"), "CH32V30x");
        assert_eq!(series_to_system_dir("ch32v307"), "CH32V30x");
        // CH32L follows the same family-directory pattern as CH32V.
        assert_eq!(series_to_system_dir("ch32l103"), "CH32L10x");
        // CH32X: exact uppercase name
        assert_eq!(series_to_system_dir("ch32x035"), "CH32X035");
    }

    #[test]
    fn test_resolve_variant_dir_falls_back_to_family_variant() {
        let tmp = tempdir();
        let fallback = tmp
            .path()
            .join("variants")
            .join("CH32V00x")
            .join("CH32V003F4");
        std::fs::create_dir_all(&fallback).unwrap();

        let resolved = resolve_variant_dir(tmp.path(), "CH32V00x/CH32V006K8", "CH32V00x");
        assert_eq!(resolved, fallback);
    }

    #[test]
    fn test_resolve_variant_h_ignores_missing_preferred_header() {
        let tmp = tempdir();
        std::fs::write(tmp.path().join("variant_CH32V003F4.h"), "").unwrap();

        let resolved = resolve_variant_h(tmp.path(), Some("variant_CH32V006K8.h"));
        assert_eq!(resolved.as_deref(), Some("variant_CH32V003F4.h"));
    }
}

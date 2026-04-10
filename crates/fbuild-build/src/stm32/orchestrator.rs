//! STM32 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (bluepill_f103c8, blackpill_f411ce, nucleo_*, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure STM32duino cores (Arduino_Core_STM32)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core + variant sources
//! 8. Compile sketch sources
//! 9. Link (with linker script from variant dir)
//! 10. Convert to hex + report size

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};
use fbuild_packages::{Framework, Toolchain};

use crate::compile_database::TargetArchitecture;
use crate::generic_arm::{ArmCompiler, ArmLinker};
use crate::pipeline;
use crate::source_scanner::SourceCollection;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

/// STM32 platform build orchestrator.
pub struct Stm32Orchestrator;

impl BuildOrchestrator for Stm32Orchestrator {
    fn platform(&self) -> Platform {
        Platform::Ststm32
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params)?;

        // 3. Ensure ARM GCC toolchain
        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("arm-gcc toolchain at {}", toolchain_dir.display());

        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        );

        if is_arduino_mbed_stm32_variant(&ctx.board.variant) {
            return build_arduino_mbed_stm32(params, ctx, &toolchain, start);
        }

        // 4. Ensure STM32duino cores
        let framework = fbuild_packages::library::Stm32Cores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("STM32 cores at {}", framework_dir.display());

        // 5. Scan sources (core + variant)
        // STM32duino uses "arduino" as its core directory name, even though
        // the board JSON says core = "stm32". Map it here.
        let core_dir = framework.get_core_dir("arduino");
        let framework_props =
            load_stm32_framework_props(&ctx.board.variant, &framework.get_boards_txt());
        let resolved_variant = framework_props
            .as_ref()
            .and_then(|props| props.get("variant").cloned())
            .unwrap_or_else(|| ctx.board.variant.clone());
        let variant_dir = framework.get_variant_dir(&resolved_variant);
        let selected_variant = select_variant_files(
            &variant_dir,
            &resolved_variant,
            framework_props
                .as_ref()
                .and_then(|props| props.get("variant_h").map(String::as_str))
                .or(ctx.board.variant_h.as_deref()),
        );

        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        // Scan core + variant, but pass None for variant — we'll filter variant
        // sources manually because the variant dir contains files for multiple
        // board variants (MALYAN, AFROFLIGHT, etc.) and startup files that
        // conflict with the generic one in cores/arduino/stm32/.
        let mut sources = scanner.scan_all(Some(&core_dir), None)?;
        sources.variant_sources = scanner
            .scan_variant_sources(&variant_dir)
            .into_iter()
            .filter(|p| keep_variant_source(p, &selected_variant))
            .collect();

        // SrcWrapper is a core library in STM32duino — its sources must be
        // compiled alongside the Arduino core (HAL wrappers, syscalls, etc.)
        // scan_core_sources is recursive, so one call covers all subdirs.
        let libs_dir = framework.get_libraries_dir();
        let srcwrapper_src = libs_dir.join("SrcWrapper").join("src");
        if srcwrapper_src.exists() {
            sources
                .core_sources
                .extend(scanner.scan_core_sources(&srcwrapper_src));
        }

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 6. Build include dirs + compiler
        // Extract MCU family from variant path (e.g. "STM32F1xx" from "STM32F1xx/F103C8T_...")
        let family = resolved_variant.split('/').next().unwrap_or("STM32F1xx");

        let mut mcu_config =
            super::mcu_config::get_stm32_config_for_mcu(&ctx.board.mcu.to_lowercase())?;
        // STM32duino linker scripts reference LD_MAX_SIZE, LD_MAX_DATA_SIZE, and
        // LD_FLASH_OFFSET as symbols. Provide them via --defsym from board config.
        let max_flash = ctx.board.max_flash.unwrap_or(65536);
        let max_ram = ctx.board.max_ram.unwrap_or(20480);
        mcu_config
            .linker_flags
            .push(format!("-Wl,--defsym=LD_MAX_SIZE={max_flash}"));
        mcu_config
            .linker_flags
            .push(format!("-Wl,--defsym=LD_MAX_DATA_SIZE={max_ram}"));
        mcu_config
            .linker_flags
            .push("-Wl,--defsym=LD_FLASH_OFFSET=0".to_string());
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        // STM32duino's stm32_def.h checks for STM32YYxx (e.g. STM32F1xx). The board
        // JSON extra_flags may only have STM32F1, so ensure the full family define.
        defines.insert(family.to_string(), "1".to_string());
        // STM32duino variant sources are guarded by ARDUINO_GENERIC_<MCU> defines.
        // Derive from the MCU name: stm32f103c8t6 → ARDUINO_GENERIC_F103C8TX
        let generic_board = stm32_generic_board_define(&ctx.board.mcu);
        defines.insert(format!("ARDUINO_{generic_board}"), "1".to_string());
        // STM32duino requires these defines for HAL/LL and variant header resolution.
        defines.insert("USE_HAL_DRIVER".to_string(), "1".to_string());
        defines.insert("USE_FULL_LL_DRIVER".to_string(), "1".to_string());
        defines.insert(
            "VARIANT_H".to_string(),
            format!("\\\"{}\\\"", selected_variant.header),
        );
        // UART HAL module is disabled by default in stm32yyxx_hal_conf.h — enable it
        // so WSerial.h can create the Serial instance.
        defines.insert("HAL_UART_MODULE_ENABLED".to_string(), "1".to_string());
        defines.insert("HAL_PCD_MODULE_ENABLED".to_string(), "1".to_string());

        // Build include dirs manually (can't use get_include_paths because
        // STM32duino core dir is "arduino", not the board JSON's "stm32")
        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone()];
        // Core subdirectories (AVR compat, STM32 HAL wrapper)
        include_dirs.push(core_dir.join("avr"));
        include_dirs.push(core_dir.join("stm32"));
        // SrcWrapper include dirs (clock.h, analog.h, LL/stm32yyxx_ll_*.h, etc.)
        let srcwrapper_inc = libs_dir.join("SrcWrapper").join("inc");
        if srcwrapper_inc.exists() {
            include_dirs.push(srcwrapper_inc.clone());
            let ll_inc = srcwrapper_inc.join("LL");
            if ll_inc.exists() {
                include_dirs.push(ll_inc);
            }
        }
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);

        // STM32duino system includes (CMSIS device, HAL drivers, etc.)
        let system_dir = framework.get_system_dir();
        add_stm32_system_includes(&system_dir, family, &mut include_dirs);

        // CMSIS Core includes (core_cm3.h, core_cm4.h, etc.) — not bundled in STM32duino
        let cmsis = fbuild_packages::library::CmsisFramework::new(&params.project_dir);
        let _cmsis_dir = fbuild_packages::Package::ensure_installed(&cmsis)?;
        tracing::info!("CMSIS framework installed");
        include_dirs.push(cmsis.get_core_include_dir());

        // Toolchain sysroot includes (ARM CMSIS headers, etc.)
        include_dirs.extend(toolchain.get_include_dirs());

        let compiler = ArmCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        );

        // 7. Create linker (linker script from variant dir)
        let linker_script = match ctx.board.ldscript.as_deref() {
            Some(name) => variant_dir.join(name),
            None => framework.get_linker_script(&resolved_variant),
        };
        let linker = ArmLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script,
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
            Some(&lib_env),
            TargetArchitecture::Arm,
            "STM32",
            start,
        )
    }
}

fn build_arduino_mbed_stm32(
    params: &BuildParams,
    ctx: pipeline::BuildContext,
    toolchain: &fbuild_packages::toolchain::ArmToolchain,
    start: Instant,
) -> Result<BuildResult> {
    let framework = fbuild_packages::library::ArduinoMbedCore::new(&params.project_dir);
    let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
    tracing::info!("Arduino mbed core at {}", framework_dir.display());

    let core_dir = framework.get_core_dir("arduino");
    let variant_dir = framework.get_variant_dir(&ctx.board.variant);

    let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
    let sources = SourceCollection {
        sketch_sources: scanner.scan_sketch_sources()?,
        core_sources: framework.get_core_sources(),
        variant_sources: framework.get_variant_sources(&ctx.board.variant),
        headers: Vec::new(),
    };

    tracing::info!(
        "sources: {} sketch, {} core, {} variant",
        sources.sketch_sources.len(),
        sources.core_sources.len(),
        sources.variant_sources.len(),
    );

    let mut defines = ctx.board.get_defines();
    defines.insert("ARDUINO".to_string(), "10810".to_string());
    defines.insert("ARDUINO_ARCH_MBED".to_string(), "1".to_string());
    for token in fbuild_core::shell_split::split(
        &framework.read_variant_file(&ctx.board.variant, "defines.txt"),
    ) {
        if let Some(def) = token.strip_prefix("-D") {
            if let Some((key, val)) = def.split_once('=') {
                defines.insert(key.to_string(), val.to_string());
            } else {
                defines.insert(def.to_string(), "1".to_string());
            }
        }
    }
    let board_ldflags = load_stm32_framework_props(&ctx.board.variant, &framework.get_boards_txt())
        .and_then(|props| props.get("extra_ldflags").cloned())
        .map(|flags| fbuild_core::shell_split::split(&flags))
        .unwrap_or_default();
    apply_define_flags(&board_ldflags, &mut defines);

    let mut include_dirs = vec![
        core_dir.clone(),
        core_dir.join("api").join("deprecated"),
        core_dir.join("api").join("deprecated-avr-comp"),
        variant_dir.clone(),
    ];
    include_dirs.extend(framework.get_variant_includes(&ctx.board.variant));
    include_dirs.push(ctx.src_dir.clone());
    pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
    dedupe_paths(&mut include_dirs);

    let mut variant_ldflags = fbuild_core::shell_split::split(
        &framework.read_variant_file(&ctx.board.variant, "ldflags.txt"),
    );
    variant_ldflags.extend(board_ldflags.iter().cloned());
    dedupe_strings(&mut variant_ldflags);
    let linker_script = preprocess_linker_script(
        toolchain.get_gxx_path(),
        &variant_dir,
        &ctx.board.variant,
        &ctx.build_dir,
        &variant_ldflags,
    )?;

    let mcu_config = build_arduino_mbed_mcu_config(
        &framework,
        &ctx.board.variant,
        framework.get_mbed_lib(&ctx.board.variant),
        &board_ldflags,
    );

    let compiler = ArmCompiler::new(
        toolchain.get_gcc_path(),
        toolchain.get_gxx_path(),
        &ctx.board.mcu,
        &ctx.board.f_cpu,
        defines,
        include_dirs.clone(),
        mcu_config.clone(),
        params.profile,
        params.verbose,
    );

    let linker = ArmLinker::new(
        toolchain.get_gcc_path(),
        toolchain.get_ar_path(),
        toolchain.get_objcopy_path(),
        toolchain.get_size_path(),
        linker_script,
        mcu_config,
        params.profile,
        ctx.board.max_flash,
        ctx.board.max_ram,
        params.verbose,
    );

    let gcc_path = toolchain.get_gcc_path();
    let gxx_path = toolchain.get_gxx_path();
    let ar_path = toolchain.get_ar_path();
    let gcc_ar_path = toolchain.get_gcc_ar_path();
    let c_flags = crate::compiler::Compiler::c_flags(&compiler);
    let cpp_flags = crate::compiler::Compiler::cpp_flags(&compiler);
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

    pipeline::run_sequential_build_with_libs(
        &compiler,
        &linker,
        ctx,
        params,
        &sources,
        Some(&lib_env),
        TargetArchitecture::Arm,
        "STM32",
        start,
    )
}

/// Add STM32duino system include directories for CMSIS and HAL.
///
/// The STM32duino core bundles CMSIS and HAL drivers under `system/`:
/// - `system/Drivers/CMSIS/Device/ST/<family>/Include/` — MCU device headers
/// - `system/Drivers/CMSIS/Core/Include/` — ARM CMSIS core headers
/// - `system/Drivers/<family>_HAL_Driver/Inc/` — STM32 HAL headers
/// - `system/<family>/` — System startup headers
fn add_stm32_system_includes(
    system_dir: &Path,
    family: &str,
    include_dirs: &mut Vec<std::path::PathBuf>,
) {
    let drivers = system_dir.join("Drivers");

    // CMSIS Device headers (stm32f1xx.h, stm32f103xb.h, etc.)
    let cmsis_device = drivers
        .join("CMSIS")
        .join("Device")
        .join("ST")
        .join(family)
        .join("Include");
    if cmsis_device.exists() {
        include_dirs.push(cmsis_device);
    }

    // CMSIS Core headers (core_cm3.h, core_cm4.h, etc.)
    let cmsis_core = drivers.join("CMSIS").join("Core").join("Include");
    if cmsis_core.exists() {
        include_dirs.push(cmsis_core);
    }

    // CMSIS startup templates (startup_stm32f103xb.s, etc.)
    let cmsis_startup = drivers
        .join("CMSIS")
        .join("Device")
        .join("ST")
        .join(family)
        .join("Source")
        .join("Templates")
        .join("gcc");
    if cmsis_startup.exists() {
        include_dirs.push(cmsis_startup);
    }

    // HAL Driver headers (stm32f1xx_hal.h, etc.)
    let hal_driver = drivers.join(format!("{family}_HAL_Driver"));
    let hal_inc = hal_driver.join("Inc");
    if hal_inc.exists() {
        include_dirs.push(hal_inc);
    }
    // HAL Driver sources — SrcWrapper's stm32yyxx_hal.c does #include "stm32f1xx_hal.c"
    let hal_src = hal_driver.join("Src");
    if hal_src.exists() {
        include_dirs.push(hal_src);
    }

    // System family directory (startup and system config headers)
    let system_family = system_dir.join(family);
    if system_family.exists() {
        include_dirs.push(system_family);
    }
}

fn is_arduino_mbed_stm32_variant(variant: &str) -> bool {
    matches!(
        variant,
        "GIGA" | "PORTENTA_H7_M7" | "GENERIC_STM32H747_M4" | "NICLA_VISION" | "OPTA"
    )
}

fn build_arduino_mbed_mcu_config(
    framework: &fbuild_packages::library::ArduinoMbedCore,
    variant_name: &str,
    mbed_lib: PathBuf,
    board_ldflags: &[String],
) -> crate::generic_arm::ArmMcuConfig {
    let cflags =
        fbuild_core::shell_split::split(&framework.read_variant_file(variant_name, "cflags.txt"));
    let cxxflags =
        fbuild_core::shell_split::split(&framework.read_variant_file(variant_name, "cxxflags.txt"));
    let ldflags =
        fbuild_core::shell_split::split(&framework.read_variant_file(variant_name, "ldflags.txt"));

    let mut common_flags: Vec<String> = cflags
        .into_iter()
        .filter(|f| f != "-c" && !f.starts_with("-std="))
        .collect();
    common_flags.push("-nostdlib".to_string());
    dedupe_strings(&mut common_flags);

    let c_flags = vec!["-std=gnu11".to_string()];
    let mut cxx_only = cxxflags
        .into_iter()
        .filter(|f| f != "-c" && !common_flags.contains(f))
        .collect::<Vec<_>>();
    dedupe_strings(&mut cxx_only);

    let mut linker_flags: Vec<String> = ldflags
        .into_iter()
        .filter(|f| !f.starts_with("-D"))
        .collect();
    linker_flags.extend(
        board_ldflags
            .iter()
            .filter(|f| !f.starts_with("-D"))
            .cloned(),
    );
    linker_flags.push("--specs=nano.specs".to_string());
    linker_flags.push("--specs=nosys.specs".to_string());
    dedupe_strings(&mut linker_flags);

    crate::generic_arm::ArmMcuConfig {
        name: "ArduinoCore-mbed".to_string(),
        description: format!("Arduino mbed variant {variant_name}"),
        architecture: "arm-cortex-m".to_string(),
        compiler_flags: crate::compiler::CompilerFlags {
            common: common_flags,
            c: c_flags,
            cxx: cxx_only,
        },
        linker_flags,
        linker_libs: vec![
            "-Wl,--whole-archive".to_string(),
            mbed_lib.to_string_lossy().to_string(),
            "-Wl,--no-whole-archive".to_string(),
            "-Wl,--start-group".to_string(),
            "-lstdc++".to_string(),
            "-lsupc++".to_string(),
            "-lm".to_string(),
            "-lc".to_string(),
            "-lgcc".to_string(),
            "-lnosys".to_string(),
            "-Wl,--end-group".to_string(),
        ],
        objcopy: crate::compiler::ObjcopyConfig {
            output_format: "binary".to_string(),
            remove_sections: Vec::new(),
        },
        profiles: HashMap::new(),
        defines: Vec::new(),
    }
}

fn preprocess_linker_script(
    gxx_path: PathBuf,
    variant_dir: &Path,
    variant_name: &str,
    build_dir: &Path,
    ldflags: &[String],
) -> Result<PathBuf> {
    let input = variant_dir.join("linker_script.ld");
    let output = build_dir.join("cpp.linker_script.ld");
    let mut args = vec![
        gxx_path.to_string_lossy().to_string(),
        "-E".to_string(),
        "-P".to_string(),
        "-x".to_string(),
        "c".to_string(),
    ];
    args.extend(ldflags.iter().filter(|f| f.starts_with("-D")).cloned());
    args.push(input.to_string_lossy().to_string());
    args.push("-o".to_string());
    args.push(output.to_string_lossy().to_string());

    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let result = fbuild_core::subprocess::run_command(&args_ref, None, None, None)?;
    if !result.success() {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "failed to preprocess Arduino mbed linker script for {}:\n{}",
            variant_name, result.stderr
        )));
    }

    Ok(output)
}

fn dedupe_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}

fn dedupe_strings(flags: &mut Vec<String>) {
    let mut seen = HashSet::new();
    flags.retain(|flag| seen.insert(flag.clone()));
}

fn apply_define_flags(flags: &[String], defines: &mut HashMap<String, String>) {
    for flag in flags {
        if let Some(def) = flag.strip_prefix("-D") {
            if let Some((key, val)) = def.split_once('=') {
                defines.insert(key.to_string(), val.to_string());
            } else {
                defines.insert(def.to_string(), "1".to_string());
            }
        }
    }
}

/// Derive the STM32duino generic board define from the MCU name.
///
/// `stm32f103c8t6` → `GENERIC_F103C8TX`
/// `stm32f411ceu6` → `GENERIC_F411CEUX`
///
/// Pattern: strip `stm32` prefix, uppercase, replace last char with `X`.
fn stm32_generic_board_define(mcu: &str) -> String {
    let suffix = mcu
        .to_lowercase()
        .strip_prefix("stm32")
        .unwrap_or(&mcu.to_lowercase())
        .to_uppercase();
    // Replace last character (pin-count digit) with X
    let mut chars: Vec<char> = suffix.chars().collect();
    if let Some(last) = chars.last_mut() {
        *last = 'X';
    }
    let trimmed: String = chars.into_iter().collect();
    format!("GENERIC_{trimmed}")
}

#[derive(Debug, Clone)]
struct SelectedVariantFiles {
    header: String,
    source_stem: Option<String>,
    peripheral_stem: Option<String>,
}

fn select_variant_files(
    variant_dir: &Path,
    variant_name: &str,
    preferred_header: Option<&str>,
) -> SelectedVariantFiles {
    let entries = std::fs::read_dir(variant_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();

    let header = preferred_header
        .and_then(|name| find_entry_case_insensitive(&entries, name))
        .or_else(|| pick_variant_file(&entries, variant_name, "variant_", ".h"))
        .unwrap_or_else(|| "variant_generic.h".to_string());

    let header_suffix = Path::new(&header)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("variant_"));
    let source_stem = header_suffix
        .and_then(|suffix| {
            find_entry_case_insensitive(&entries, &format!("variant_{suffix}.cpp"))
                .map(|name| stem_lower(&name))
        })
        .or_else(|| {
            pick_variant_file(&entries, variant_name, "variant_", ".cpp")
                .map(|name| stem_lower(&name))
        });
    let peripheral_stem = header_suffix
        .and_then(|suffix| {
            find_entry_case_insensitive(&entries, &format!("PeripheralPins_{suffix}.c"))
                .or_else(|| {
                    find_entry_case_insensitive(&entries, &format!("peripheralpins_{suffix}.c"))
                })
                .map(|name| stem_lower(&name))
        })
        .or_else(|| {
            pick_variant_file(&entries, variant_name, "peripheralpins_", ".c")
                .map(|name| stem_lower(&name))
        });

    SelectedVariantFiles {
        header,
        source_stem,
        peripheral_stem,
    }
}

fn pick_variant_file(
    entries: &[String],
    variant_name: &str,
    prefix: &str,
    suffix: &str,
) -> Option<String> {
    let normalized = normalize_variant_name(variant_name);
    let exact = format!("{prefix}{normalized}{suffix}");
    if let Some(name) = find_entry_case_insensitive(entries, &exact) {
        return Some(name);
    }

    let generic = format!("{prefix}generic{suffix}");
    if let Some(name) = find_entry_case_insensitive(entries, &generic) {
        return Some(name);
    }

    let mut matches = entries
        .iter()
        .filter(|name| {
            let lower = name.to_lowercase();
            lower.starts_with(prefix) && lower.ends_with(suffix)
        })
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by_key(|name| name.to_lowercase());
    matches.into_iter().next()
}

fn find_entry_case_insensitive(entries: &[String], target: &str) -> Option<String> {
    entries
        .iter()
        .find(|name| name.eq_ignore_ascii_case(target))
        .cloned()
}

fn keep_variant_source(path: &Path, selected: &SelectedVariantFiles) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let stem = stem_lower(&name);

    if name.starts_with("startup_") {
        return false;
    }
    if name.starts_with("variant_") {
        return selected
            .source_stem
            .as_ref()
            .is_some_and(|wanted| &stem == wanted);
    }
    if name.starts_with("peripheralpins_") {
        return selected
            .peripheral_stem
            .as_ref()
            .is_some_and(|wanted| &stem == wanted);
    }

    true
}

fn normalize_variant_name(name: &str) -> String {
    name.to_lowercase()
        .replace(['/', '\\', '-', ' '], "_")
        .replace("__", "_")
}

fn stem_lower(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}

fn load_stm32_framework_props(
    board_or_variant: &str,
    boards_txt: &Path,
) -> Option<HashMap<String, String>> {
    let content = std::fs::read_to_string(boards_txt).ok()?;
    let preferred_key = if board_or_variant.contains('/') {
        ".build.variant"
    } else {
        ".build.board"
    };
    let prefix = find_stm32_prop_prefix(&content, preferred_key, board_or_variant)
        .or_else(|| find_stm32_prop_prefix(&content, ".build.variant", board_or_variant))?;

    let mut props = HashMap::new();
    for scope in stm32_property_scopes(&prefix) {
        let line_prefix = format!("{scope}.");
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some(rest) = trimmed.strip_prefix(&line_prefix) else {
                continue;
            };
            let Some((key, value)) = rest.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let normalized = key
                .strip_prefix("build.")
                .or_else(|| key.strip_prefix("upload."))
                .unwrap_or(key);
            props.insert(normalized.to_string(), value.trim().to_string());
            if normalized != key {
                props.insert(key.to_string(), value.trim().to_string());
            }
        }
    }

    let substitutions = [
        (
            "{build.board}",
            props.get("board").cloned().unwrap_or_default(),
        ),
        (
            "{build.variant}",
            props.get("variant").cloned().unwrap_or_default(),
        ),
    ];
    for value in props.values_mut() {
        for (needle, replacement) in &substitutions {
            if !replacement.is_empty() {
                *value = value.replace(needle, replacement);
            }
        }
    }

    Some(props)
}

fn find_stm32_prop_prefix(content: &str, suffix: &str, value: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        let (key, actual) = trimmed.split_once('=')?;
        if key.ends_with(suffix) && actual.trim() == value {
            Some(key.trim_end_matches(suffix).to_string())
        } else {
            None
        }
    })
}

fn stm32_property_scopes(prefix: &str) -> Vec<String> {
    let segments = prefix.split('.').collect::<Vec<_>>();
    if segments.is_empty() {
        return Vec::new();
    }

    let mut scopes = vec![segments[0].to_string()];
    let mut idx = 1;
    while idx + 2 < segments.len() {
        if segments[idx] != "menu" {
            break;
        }
        idx += 3;
        scopes.push(segments[..idx].join("."));
    }

    if scopes.last().is_none_or(|scope| scope != prefix) {
        scopes.push(prefix.to_string());
    }

    scopes
}

/// Create an STM32 orchestrator.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Stm32Orchestrator)
}

/// Check if a project is configured for STM32.
pub fn is_stm32_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::Ststm32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_stm32_orchestrator_platform() {
        let orch = Stm32Orchestrator;
        assert_eq!(orch.platform(), Platform::Ststm32);
    }

    #[test]
    fn test_is_stm32_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:bluepill]\nplatform = ststm32\nboard = bluepill_f103c8\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_stm32_project(tmp.path(), "bluepill"));
        assert!(!is_stm32_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_load_stm32_framework_props_resolves_variant_h_template() {
        let tmp = tempfile::TempDir::new().unwrap();
        let boards_txt = tmp.path().join("boards.txt");
        fs::write(
            &boards_txt,
            "\
GenF1.build.variant_h=variant_{build.board}.h
GenF1.menu.pnum.MAPLEMINI_F103CB.build.board=MAPLEMINI_F103CB
GenF1.menu.pnum.MAPLEMINI_F103CB.build.variant=STM32F1xx/F103C8T_F103CB(T-U)
giga.menu.target_core.cm7.build.variant=GIGA
giga.build.extra_ldflags=-DCM4_BINARY_START=0x08180000
",
        )
        .unwrap();

        let maple = load_stm32_framework_props("MAPLEMINI_F103CB", &boards_txt).unwrap();
        assert_eq!(
            maple.get("variant_h").map(String::as_str),
            Some("variant_MAPLEMINI_F103CB.h")
        );

        let giga = load_stm32_framework_props("GIGA", &boards_txt).unwrap();
        assert_eq!(
            giga.get("extra_ldflags").map(String::as_str),
            Some("-DCM4_BINARY_START=0x08180000")
        );
    }

    #[test]
    fn test_stm32_property_scopes_include_parent_menu_levels() {
        assert_eq!(
            stm32_property_scopes("giga.menu.target_core.cm7"),
            vec!["giga", "giga.menu.target_core.cm7"]
        );
        assert_eq!(
            stm32_property_scopes("foo.menu.cpu.atmega328.menu.speed.fast"),
            vec![
                "foo",
                "foo.menu.cpu.atmega328",
                "foo.menu.cpu.atmega328.menu.speed.fast"
            ]
        );
    }

    #[test]
    fn test_select_variant_files_prefers_framework_header() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::write(tmp.path().join("variant_MAPLEMINI_F103CB.h"), "").unwrap();
        fs::write(tmp.path().join("variant_MAPLEMINI_F103CB.cpp"), "").unwrap();
        fs::write(tmp.path().join("variant_generic.cpp"), "").unwrap();

        let selected = select_variant_files(
            tmp.path(),
            "STM32F1xx/F103C8T_F103CB(T-U)",
            Some("variant_MAPLEMINI_F103CB.h"),
        );

        assert_eq!(selected.header, "variant_MAPLEMINI_F103CB.h");
        assert_eq!(
            selected.source_stem.as_deref(),
            Some("variant_maplemini_f103cb")
        );
    }
}

//! Arduino-mbed build path for STM32-based mbed variants (GIGA, PORTENTA, ...).
//!
//! Extracted from `orchestrator.rs` (see [`super`]). The STM32duino core
//! doesn't cover these boards — Arduino ships its own pre-built mbed library
//! plus per-variant `cflags.txt`, `cxxflags.txt`, and `ldflags.txt` files
//! that we replay verbatim.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::Result;
use fbuild_packages::Toolchain;

use crate::compile_database::TargetArchitecture;
use crate::generic_arm::{ArmCompiler, ArmLinker};
use crate::pipeline;
use crate::source_scanner::SourceCollection;
use crate::{BuildParams, BuildResult, SourceScanner};

use super::framework_props::load_stm32_framework_props;
use super::includes::{apply_define_flags, dedupe_paths, dedupe_strings};

pub(super) fn is_arduino_mbed_stm32_variant(variant: &str) -> bool {
    matches!(
        variant,
        "GIGA" | "PORTENTA_H7_M7" | "GENERIC_STM32H747_M4" | "NICLA_VISION" | "OPTA"
    )
}

pub(super) fn build_arduino_mbed_stm32(
    params: &BuildParams,
    ctx: pipeline::BuildContext,
    toolchain: &fbuild_packages::toolchain::ArmToolchain,
    start: Instant,
) -> Result<BuildResult> {
    // Compute eh_frame strip policy once per build (FastLED/fbuild#244).
    let eh_frame_policy =
        crate::eh_frame_policy_compute::compute_eh_frame_policy(&ctx, params.profile, None);

    let framework = fbuild_packages::library::ArduinoMbedCore::new(&params.project_dir);
    let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
    tracing::info!("Arduino mbed core at {}", framework_dir.display());

    let core_dir = framework.get_core_dir("arduino");
    let variant_dir = framework.get_variant_dir(&ctx.board.variant);

    let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
    let sources = SourceCollection {
        sketch_sources: scanner.scan_sketch_sources_filtered(ctx.source_filter.as_deref())?,
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
    )
    .with_build_unflags(ctx.build_unflags.clone())
    .with_eh_frame_policy(eh_frame_policy);

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
        &[],
        Some(&lib_env),
        TargetArchitecture::Arm,
        "STM32",
        start,
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

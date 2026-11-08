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

#[expect(clippy::too_many_lines, reason = "baseline, zackees/ci.yml#229")]
pub(super) async fn build_arduino_mbed_stm32(
    params: &BuildParams,
    ctx: pipeline::BuildContext,
    toolchain: &fbuild_packages::toolchain::ArmToolchain,
    framework: fbuild_packages::library::ArduinoMbedCore,
    start: Instant,
) -> Result<BuildResult> {
    // Compute eh_frame strip policy once per build (FastLED/fbuild#244).
    let eh_frame_policy =
        crate::eh_frame_policy_compute::compute_eh_frame_policy(&ctx, params.profile, None);

    // `framework` honors the `framework-arduino-mbed` `platform_packages`
    // override (FastLED/fbuild#664, #681); see `super::stm32_packages`.
    let framework_dir = fbuild_packages::Package::ensure_installed(&framework).await?;
    tracing::info!("Arduino mbed core at {}", framework_dir.display());

    let core_dir = framework.get_core_dir("arduino");
    let variant_dir = framework.get_variant_dir(&ctx.board.variant);

    let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
    let (sketch_sources, ino_preludes) = scanner
        .scan_sketch_sources_filtered_with_include_roots_and_preludes(
            ctx.source_filter.as_deref(),
            &[core_dir.as_path(), variant_dir.as_path()],
        )?;
    let sources = SourceCollection {
        sketch_sources,
        core_sources: framework.get_core_sources(),
        variant_sources: framework.get_variant_sources(&ctx.board.variant),
        headers: Vec::new(),
        ino_preludes,
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
    pipeline::discover_project_includes(&params.project_dir, &mut include_dirs)?;
    dedupe_paths(&mut include_dirs);

    let mut variant_ldflags = fbuild_core::shell_split::split(
        &framework.read_variant_file(&ctx.board.variant, "ldflags.txt"),
    );
    variant_ldflags.extend(board_ldflags.iter().cloned());
    apply_mbed_flash_layout(
        &ctx.board.board_id,
        ctx.config.get_env_config(&params.env_name).ok(),
        &mut defines,
        &mut variant_ldflags,
    )?;
    dedupe_strings(&mut variant_ldflags);
    let linker_script = preprocess_linker_script(
        toolchain.get_gxx_path(),
        &variant_dir,
        &ctx.board.variant,
        &ctx.build_dir,
        &variant_ldflags,
    )
    .await?;

    let mcu_config = build_arduino_mbed_mcu_config(
        &framework,
        &ctx.board.variant,
        framework.get_mbed_lib(&ctx.board.variant),
        &board_ldflags,
    );

    // Download `lib_deps` from the registry / remote URLs before
    // creating the compiler, so the downloaded library include directories
    // are available during compilation (FastLED/fbuild#1276).
    let lib_deps = ctx.config.get_lib_deps(&params.env_name)?;
    let lib_ignore = ctx
        .config
        .get_lib_ignore(&params.env_name)
        .unwrap_or_default();
    let lib_deps_plan = if !lib_deps.is_empty() {
        let temp_compiler = ArmCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines.clone(),
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        );
        pipeline::resolve_lib_deps(
            &lib_deps,
            &lib_ignore,
            &params.project_dir,
            &ctx.build_dir,
            &toolchain.get_gcc_path(),
            &toolchain.get_gxx_path(),
            &toolchain.get_ar_path(),
            &toolchain.get_gcc_ar_path(),
            &crate::compiler::Compiler::c_flags(&temp_compiler),
            &crate::compiler::Compiler::cpp_flags(&temp_compiler),
            &mut include_dirs,
            params.verbose,
            None,
        )
        .await?
    } else {
        pipeline::LibDeps::default()
    };

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
        lib_deps_plan,
        Some(&lib_env),
        TargetArchitecture::Arm,
        "STM32",
        start,
    )
    .await
}

/// Match PlatformIO's ArduinoCore-mbed flash partition selection. Its
/// packaged GIGA/Portenta linker scripts and variant sources reference these
/// symbols, but the default layout is encoded in the platform builder rather
/// than in the downloaded framework archive.
#[derive(serde::Deserialize)]
struct MbedFlashLayoutConfig {
    boards: Vec<String>,
    m4_boards: Vec<String>,
    external_ram_m4_boards: Vec<String>,
    default_layout: String,
    external_ram_layout: String,
    cm4_binary_start: HashMap<String, String>,
    cm4_binary_end: String,
    external_cm4_binary_end: String,
    external_cm4_ram_end: String,
}

#[derive(serde::Deserialize)]
struct Stm32H7FlashConfig {
    arduino_mbed_flash: MbedFlashLayoutConfig,
}

fn mbed_flash_layout_config() -> Result<MbedFlashLayoutConfig> {
    serde_json::from_str::<Stm32H7FlashConfig>(include_str!("../configs/stm32h7.json"))
        .map(|config| config.arduino_mbed_flash)
        .map_err(|error| {
            fbuild_core::FbuildError::ConfigError(format!(
                "invalid STM32H7 Arduino Mbed flash configuration: {error}"
            ))
        })
}

fn apply_mbed_flash_layout(
    board_id: &str,
    env_config: Option<&HashMap<String, String>>,
    defines: &mut HashMap<String, String>,
    linker_flags: &mut Vec<String>,
) -> Result<()> {
    let config = mbed_flash_layout_config()?;
    if !config.boards.iter().any(|board| board == board_id) {
        return Ok(());
    }
    let layout = env_config
        .and_then(|env| env.get("board_build.arduino.flash_layout"))
        .map(String::as_str)
        .unwrap_or(&config.default_layout);
    let cm4_start = config.cm4_binary_start.get(layout).ok_or_else(|| {
        fbuild_core::FbuildError::ConfigError(format!(
            "unsupported Arduino Mbed flash layout `{layout}`"
        ))
    })?;
    defines.insert("CM4_BINARY_START".into(), cm4_start.into());
    linker_flags.push(format!("-DCM4_BINARY_START={cm4_start}"));
    if config.m4_boards.iter().any(|board| board == board_id) {
        let external_ram = layout == config.external_ram_layout
            && config
                .external_ram_m4_boards
                .iter()
                .any(|board| board == board_id);
        let cm4_end = if external_ram {
            Some(&config.external_cm4_binary_end)
        } else if layout == config.external_ram_layout {
            None
        } else {
            Some(&config.cm4_binary_end)
        };
        if let Some(cm4_end) = cm4_end {
            defines.insert("CM4_BINARY_END".into(), cm4_end.into());
            linker_flags.push(format!("-DCM4_BINARY_END={cm4_end}"));
        }
        if external_ram {
            defines.insert("CM4_RAM_END".into(), config.external_cm4_ram_end.clone());
            linker_flags.push(format!("-DCM4_RAM_END={}", config.external_cm4_ram_end));
        }
    }
    Ok(())
}

fn build_arduino_mbed_mcu_config(
    framework: &fbuild_packages::library::ArduinoMbedCore,
    variant_name: &str,
    mbed_lib: PathBuf,
    board_ldflags: &[String],
) -> crate::generic_arm::ArmMcuConfig {
    arduino_mbed_mcu_config_from_variant_text(
        variant_name,
        &framework.read_variant_file(variant_name, "cflags.txt"),
        &framework.read_variant_file(variant_name, "cxxflags.txt"),
        &framework.read_variant_file(variant_name, "ldflags.txt"),
        mbed_lib,
        board_ldflags,
    )
}

/// Build the mbed MCU config from the variant's three flag files, verbatim.
///
/// Split out from [`build_arduino_mbed_mcu_config`] so the flag policy can be
/// asserted without an installed `ArduinoCore-mbed` on disk.
fn arduino_mbed_mcu_config_from_variant_text(
    variant_name: &str,
    cflags_text: &str,
    cxxflags_text: &str,
    ldflags_text: &str,
    mbed_lib: PathBuf,
    board_ldflags: &[String],
) -> crate::generic_arm::ArmMcuConfig {
    let cflags = fbuild_core::shell_split::split(cflags_text);
    let cxxflags = fbuild_core::shell_split::split(cxxflags_text);
    let ldflags = fbuild_core::shell_split::split(ldflags_text);

    let mut common_flags: Vec<String> = cflags
        .into_iter()
        .filter(|f| f != "-c" && !f.starts_with("-std="))
        .collect();
    common_flags.push("-nostdlib".to_string());
    // ArduinoCore-mbed's `variants/<name>/cflags.txt` is only the *body* of
    // Arduino's compile recipe: the `-MMD` that makes gcc emit a per-TU `.d`
    // sits in `platform.txt`'s recipe, which this path never replays. Every
    // other MCU config carries `-MMD` in its JSON, so without this line the
    // mbed path is the only one that produces ZERO depfiles and
    // `CompilerBase::needs_rebuild` degrades to a source-mtime comparison --
    // and the source it stats is the *generated* `main.cpp`, whose mtime does
    // not move when an included sketch tab or header is edited. Editing an
    // included file therefore kept reporting a stale "compilation succeeded"
    // (FastLED/fbuild#1948).
    common_flags.push("-MMD".to_string());
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

async fn preprocess_linker_script(
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
    // FastLED/fbuild#809: linker-script preprocessing is a trivial
    // g++ -E invocation; bound to 30s.
    //
    // Deliberately keeps the inherited cwd (FastLED/fbuild#1269): this is not
    // a link. `-E -P` stops after preprocessing, so no `collect2` /
    // `lto-wrapper` is spawned and nothing writes cwd-relative scratch files —
    // the stray `-r` class of junk cannot originate here. Both the input
    // (`variant_dir`) and the output (`build_dir`) are already explicit paths,
    // so the cwd does not affect where anything lands. The `None` env is
    // likewise correct: `link_env_for_build` only redirects GCC's *LTO* temp
    // dir, which a preprocess-only run never uses.
    let result = fbuild_core::subprocess::run_command(
        &args_ref,
        None,
        None,
        Some(std::time::Duration::from_secs(30)),
    )
    .await?;
    if !result.success() {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "failed to preprocess Arduino mbed linker script for {}:\n{}",
            variant_name, result.stderr
        )));
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim shape of `ArduinoCore-mbed 4.5.0/variants/GIGA/cflags.txt`:
    /// note the ABSENCE of `-MMD` — Arduino puts it in `platform.txt`'s
    /// recipe, which this build path never replays.
    const GIGA_CFLAGS: &str = "-c -std=gnu11 -DAPPLICATION_ADDR=0x8040000 -Os -Wall \
-Wextra -Wno-missing-field-initializers -fdata-sections -ffunction-sections -mcpu=cortex-m7 \
-mfloat-abi=softfp -mfpu=fpv5-d16 -mthumb";
    const GIGA_CXXFLAGS: &str =
        "-Wvla -c -fno-rtti -std=gnu++14 -Os -Wall -mcpu=cortex-m7 -mfloat-abi=softfp -mthumb";
    const GIGA_LDFLAGS: &str = "-mcpu=cortex-m7 -mthumb -Wl,--gc-sections --specs=nano.specs";

    fn giga_config() -> crate::generic_arm::ArmMcuConfig {
        arduino_mbed_mcu_config_from_variant_text(
            "GIGA",
            GIGA_CFLAGS,
            GIGA_CXXFLAGS,
            GIGA_LDFLAGS,
            PathBuf::from("/framework/libmbed.a"),
            &[],
        )
    }

    /// FastLED/fbuild#1948: without `-MMD` gcc writes no `.d`, so
    /// `CompilerBase::needs_rebuild` only compares the *generated* `main.cpp`
    /// against the object and an edited sketch tab or header never
    /// invalidates it — the build reports success with stale objects.
    #[test]
    fn mbed_flags_request_a_depfile_so_included_files_invalidate() {
        let config = giga_config();
        let common = &config.compiler_flags.common;
        assert_eq!(
            common.iter().filter(|flag| *flag == "-MMD").count(),
            1,
            "mbed common flags must request exactly one depfile: {common:?}"
        );
        // `-c` is a driver flag and `-std=` is language-specific; neither may
        // leak out of cflags.txt into the shared block.
        assert!(!common.iter().any(|flag| flag == "-c"), "{common:?}");
        assert!(
            !common.iter().any(|flag| flag.starts_with("-std=")),
            "{common:?}"
        );
        // The flag has to survive into both language-specific sets gcc sees.
        for flags in [
            crate::compiler::build_c_flags(common.clone(), &config),
            crate::compiler::build_cpp_flags(common.clone(), &config),
        ] {
            assert!(flags.iter().any(|flag| flag == "-MMD"), "{flags:?}");
        }
    }

    /// The mbed path is the only one that builds flags from text; every other
    /// MCU config is a JSON file checked in beside its platform module. Guard
    /// the whole set so a new platform cannot ship without dependency tracking.
    #[test]
    fn every_mcu_config_json_requests_a_depfile() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut checked = 0;
        for entry in walkdir::WalkDir::new(&src).into_iter().flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(path).unwrap();
            let config: serde_json::Value = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let groups = config["compiler_flags"]
                .as_object()
                .unwrap_or_else(|| panic!("{}: no compiler_flags block", path.display()));
            let declares_depfile = ["common", "c", "cxx"].iter().any(|group| {
                groups
                    .get(*group)
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|list| list.iter().any(|flag| flag.as_str() == Some("-MMD")))
            });
            assert!(
                declares_depfile,
                "{} declares no -MMD, so its builds emit no depfiles and reuse \
                 objects across header/sketch edits (FastLED/fbuild#1948)",
                path.display()
            );
            checked += 1;
        }
        assert!(
            checked >= 20,
            "expected the full MCU config set, found {checked}"
        );
    }

    #[test]
    fn giga_flash_layout_matches_platformio_default_and_override() {
        for (layout, expected) in [
            (None, "0x08100000"),
            (Some("75_25"), "0x08180000"),
            (Some("100_0"), "0x60000000"),
        ] {
            let env = layout.map(|value| {
                HashMap::from([("board_build.arduino.flash_layout".into(), value.into())])
            });
            let mut defines = HashMap::new();
            let mut linker_flags = Vec::new();
            apply_mbed_flash_layout("giga_r1_m7", env.as_ref(), &mut defines, &mut linker_flags)
                .unwrap();
            assert_eq!(
                defines.get("CM4_BINARY_START").map(String::as_str),
                Some(expected)
            );
            assert!(linker_flags.contains(&format!("-DCM4_BINARY_START={expected}")));
        }
    }

    #[test]
    fn m4_flash_layout_uses_board_configured_external_ram_addresses() {
        let env = HashMap::from([("board_build.arduino.flash_layout".into(), "100_0".into())]);
        let mut defines = HashMap::new();
        let mut linker_flags = Vec::new();
        apply_mbed_flash_layout("giga_r1_m4", Some(&env), &mut defines, &mut linker_flags).unwrap();
        assert_eq!(defines["CM4_BINARY_START"], "0x60000000");
        assert_eq!(defines["CM4_BINARY_END"], "0x60040000");
        assert_eq!(defines["CM4_RAM_END"], "0x60080000");

        let mut nicla_defines = HashMap::new();
        apply_mbed_flash_layout(
            "nicla_vision_m4",
            Some(&env),
            &mut nicla_defines,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(!nicla_defines.contains_key("CM4_BINARY_END"));
        assert!(!nicla_defines.contains_key("CM4_RAM_END"));
    }

    #[test]
    fn invalid_mbed_flash_layout_fails_before_compile() {
        let env = HashMap::from([("board_build.arduino.flash_layout".into(), "bad".into())]);
        let error = apply_mbed_flash_layout(
            "giga_r1_m7",
            Some(&env),
            &mut HashMap::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported Arduino Mbed flash layout")
        );
    }
}

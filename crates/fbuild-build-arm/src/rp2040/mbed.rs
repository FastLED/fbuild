//! RP2040 ArduinoCore-mbed build recipe (FastLED/fbuild#1977).
//!
//! Unlike Arduino-Pico, Mbed ships a small wrapper core and a prebuilt
//! per-variant libmbed.a. Replay that variant's flags and linker script;
//! never substitute the Arduino-Pico SDK or its cached core objects.
//! The STM32 Mbed recipe has additional board-specific flash partition logic;
//! this RP recipe deliberately owns its independent platform boundary.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use fbuild_core::Result;
use fbuild_core::path::NormalizedPath;
use fbuild_packages::{Framework, Package, Toolchain};

use super::mbed_libraries;

use crate::compile_database::TargetArchitecture;
use crate::generic_arm::{ArmCompiler, ArmLinker, ArmMcuConfig};
use crate::pipeline;
use crate::source_scanner::SourceCollection;
use crate::{BuildParams, BuildResult, SourceScanner};

pub(super) fn packages(
    project: &Path,
    env: &HashMap<String, String>,
) -> (
    fbuild_packages::toolchain::ArmToolchain,
    fbuild_packages::library::ArduinoMbedCore,
    fbuild_packages::toolchain::Rp2040Picotool,
) {
    let toolchain = match crate::package_override::resolve_override(env, "toolchain-gccarmnoneeabi")
    {
        Some(pin) => fbuild_packages::toolchain::ArmToolchain::with_override(project, pin),
        None => fbuild_packages::toolchain::ArmToolchain::new(project),
    };
    let framework = match crate::package_override::resolve_override(env, "framework-arduino-mbed") {
        Some(pin) => fbuild_packages::library::ArduinoMbedCore::with_override(project, pin),
        None => fbuild_packages::library::ArduinoMbedCore::new(project),
    };
    let picotool =
        match crate::package_override::resolve_override(env, "tool-picotool-rp2040-earlephilhower")
        {
            Some(pin) => fbuild_packages::toolchain::Rp2040Picotool::with_override(project, pin),
            None => fbuild_packages::toolchain::Rp2040Picotool::new(project),
        };
    (toolchain, framework, picotool)
}

fn variant_text(variant: &Path, name: &str) -> Result<String> {
    let path = variant.join(name);
    std::fs::read_to_string(&path).map_err(|error| {
        fbuild_core::FbuildError::ConfigError(format!(
            "missing Mbed variant input {}: {error}",
            path.display()
        ))
    })
}

fn mcu_config(variant: &Path, name: &str) -> Result<ArmMcuConfig> {
    let c = variant_text(variant, "cflags.txt")?;
    let cpp = variant_text(variant, "cxxflags.txt")?;
    let ld = variant_text(variant, "ldflags.txt")?;
    let mut common: Vec<String> = fbuild_core::shell_split::split(&c)
        .into_iter()
        .filter(|flag| flag != "-c" && !flag.starts_with("-std="))
        .collect();
    common.extend(["-nostdlib".to_string(), "-MMD".to_string()]);
    let cxx = fbuild_core::shell_split::split(&cpp)
        .into_iter()
        .filter(|flag| flag != "-c" && !common.contains(flag))
        .collect();
    let mut linker_flags: Vec<String> = fbuild_core::shell_split::split(&ld)
        .into_iter()
        .filter(|flag| !flag.starts_with("-D"))
        .collect();
    linker_flags.extend([
        "-Wl,--as-needed".to_string(),
        "--specs=nosys.specs".to_string(),
    ]);
    let mbed_lib = variant.join("libs/libmbed.a");
    if !mbed_lib.is_file() || !variant.join("linker_script.ld").is_file() {
        return Err(fbuild_core::FbuildError::ConfigError(format!(
            "Mbed variant {name} must provide linker_script.ld and libs/libmbed.a"
        )));
    }
    Ok(ArmMcuConfig {
        name: "ArduinoCore-mbed RP2040".to_string(),
        description: format!("Arduino Mbed RP2040 variant {name}"),
        architecture: "arm-cortex-m".to_string(),
        compiler_flags: crate::compiler::CompilerFlags {
            common,
            c: vec!["-std=gnu11".to_string()],
            cxx,
        },
        linker_flags,
        linker_libs: vec![
            "-Wl,--whole-archive".to_string(),
            mbed_lib.to_string_lossy().into_owned(),
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
    })
}

fn defines(ctx: &pipeline::BuildContext, variant: &Path) -> Result<HashMap<String, String>> {
    let mut values = ctx.board.get_defines();
    values.insert("ARDUINO".to_string(), "10810".to_string());
    values.insert("ARDUINO_ARCH_MBED".to_string(), "1".to_string());
    values.insert("ARDUINO_ARCH_MBED_RP2040".to_string(), "1".to_string());
    for input in ["defines.txt", "ldflags.txt"] {
        for flag in fbuild_core::shell_split::split(&variant_text(variant, input)?) {
            if let Some(definition) = flag.strip_prefix("-D") {
                let (key, value) = definition.split_once('=').unwrap_or((definition, "1"));
                values.insert(key.to_string(), value.to_string());
            }
        }
    }
    Ok(values)
}

/// Keep the SDK's relative include recipe instead of expanding hundreds of
/// absolute paths. Expansion can overflow GCC's COLLECT_GCC_OPTIONS environment
/// entry when compiling a library, even if the driver itself used a response file.
fn sdk_include_flags(core: &Path, variant: &Path) -> Result<Vec<String>> {
    let mut flags = vec!["-iprefix".to_string(), core.to_string_lossy().into_owned()];
    flags.extend(fbuild_core::shell_split::split(&variant_text(
        variant,
        "includes.txt",
    )?));
    Ok(flags)
}

fn sources(
    ctx: &pipeline::BuildContext,
    framework: &fbuild_packages::library::ArduinoMbedCore,
) -> Result<SourceCollection> {
    let core = framework.get_core_dir("arduino");
    let variant = framework.get_variant_dir(&ctx.board.variant);
    let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
    let (sketch_sources, ino_preludes) = scanner
        .scan_sketch_sources_filtered_with_include_roots_and_preludes(
            ctx.source_filter.as_deref(),
            &[core.as_path(), variant.as_path()],
        )?;
    Ok(SourceCollection {
        sketch_sources,
        core_sources: framework.get_core_sources(),
        variant_sources: framework.get_variant_sources(&ctx.board.variant),
        headers: Vec::new(),
        ino_preludes,
    })
}

fn include_dirs(
    core: &Path,
    variant: &Path,
    ctx: &pipeline::BuildContext,
    project: &Path,
) -> Result<Vec<NormalizedPath>> {
    // Normalize once after the legacy discovery API has populated its vector.
    let mut paths = vec![
        core.to_path_buf(),
        core.join("api/deprecated"),
        core.join("api/deprecated-avr-comp"),
        variant.to_path_buf(),
        ctx.src_dir.clone(),
    ];
    pipeline::discover_project_includes(project, &mut paths)?;
    Ok(paths.into_iter().map(NormalizedPath::new).collect())
}

fn make_compiler(
    params: &BuildParams,
    ctx: &pipeline::BuildContext,
    toolchain: &fbuild_packages::toolchain::ArmToolchain,
    values: HashMap<String, String>,
    includes: &[NormalizedPath],
    config: ArmMcuConfig,
) -> ArmCompiler {
    ArmCompiler::new(
        toolchain.get_gcc_path(),
        toolchain.get_gxx_path(),
        &ctx.board.mcu,
        &ctx.board.f_cpu,
        values,
        includes
            .iter()
            .map(|path| path.as_path().to_path_buf())
            .collect(),
        config,
        params.profile,
        params.verbose,
    )
}

pub(super) async fn build(
    params: &BuildParams,
    ctx: pipeline::BuildContext,
    start: Instant,
) -> Result<BuildResult> {
    let (toolchain, framework, picotool) = packages(
        &params.project_dir,
        ctx.config.get_env_config(&params.env_name)?,
    );
    toolchain.ensure_installed().await?;
    framework.ensure_installed().await?;
    let core = framework.get_core_dir("arduino");
    let variant = framework.get_variant_dir(&ctx.board.variant);
    let mut config = mcu_config(&variant, &ctx.board.variant)?;
    config
        .compiler_flags
        .common
        .extend(sdk_include_flags(&core, &variant)?);
    let mut includes = include_dirs(&core, &variant, &ctx, &params.project_dir)?;
    let mut sources = sources(&ctx, &framework)?;
    let flags = defines(&ctx, &variant)?;
    let mut compiler = make_compiler(
        params,
        &ctx,
        &toolchain,
        flags.clone(),
        &includes,
        config.clone(),
    );
    let declared = ctx.config.get_lib_deps(&params.env_name)?;
    let libraries = fbuild_packages::library::framework_library::discover_framework_libraries(
        &framework.get_libraries_dir(),
    );
    let (_, cpp_flags) = mbed_libraries::flags(&compiler, &ctx, true);
    let selected_defines = fbuild_core::compiler_flags::parse_define_flags(&cpp_flags);
    let bundled = mbed_libraries::bundled(
        &params.project_dir,
        &ctx.src_dir,
        &libraries,
        &selected_defines,
        &declared,
        ctx.lib_archive,
    )?;
    includes.extend(bundled.includes);
    sources.core_sources.extend(
        bundled
            .sources
            .into_iter()
            .map(NormalizedPath::into_path_buf),
    );
    let plan = mbed_libraries::dependencies(
        params,
        &ctx,
        &compiler,
        &toolchain,
        &mut includes,
        &bundled.external,
    )
    .await?;
    let policy =
        crate::eh_frame_policy_compute::compute_eh_frame_policy(&ctx, params.profile, None);
    compiler = make_compiler(params, &ctx, &toolchain, flags, &includes, config.clone())
        .with_build_unflags(ctx.build_unflags.clone())
        .with_eh_frame_policy(policy);
    let linker = ArmLinker::new(
        toolchain.get_gcc_path(),
        toolchain.get_ar_path(),
        toolchain.get_objcopy_path(),
        toolchain.get_size_path(),
        variant.join("linker_script.ld"),
        config,
        params.profile,
        ctx.board.max_flash,
        ctx.board.max_ram,
        params.verbose,
    );
    let project_flags = mbed_libraries::flags(&compiler, &ctx, true);
    let prepared = Prepared {
        archives: bundled.archives,
        project_flags,
        compiler,
        linker,
        sources,
        plan,
        includes,
        picotool,
    };
    finish(params, ctx, &toolchain, prepared, start).await
}

struct Prepared {
    archives: Vec<NormalizedPath>,
    project_flags: (Vec<String>, Vec<String>),
    compiler: ArmCompiler,
    linker: ArmLinker,
    sources: SourceCollection,
    plan: pipeline::LibDeps,
    includes: Vec<NormalizedPath>,
    picotool: fbuild_packages::toolchain::Rp2040Picotool,
}

async fn finish(
    params: &BuildParams,
    ctx: pipeline::BuildContext,
    toolchain: &fbuild_packages::toolchain::ArmToolchain,
    prepared: Prepared,
    start: Instant,
) -> Result<BuildResult> {
    let Prepared {
        archives,
        project_flags,
        compiler,
        linker,
        sources,
        plan,
        includes,
        picotool,
    } = prepared;
    let gcc = toolchain.get_gcc_path();
    let gxx = toolchain.get_gxx_path();
    let ar = toolchain.get_ar_path();
    let gcc_ar = toolchain.get_gcc_ar_path();
    let (c, cpp) = project_flags;
    // Adapt normalized owned state only at the legacy shared pipeline boundary.
    let includes: Vec<_> = includes
        .into_iter()
        .map(NormalizedPath::into_path_buf)
        .collect();
    let archives: Vec<_> = archives
        .into_iter()
        .map(NormalizedPath::into_path_buf)
        .collect();
    let env = pipeline::LibraryBuildEnv {
        gcc_path: &gcc,
        gxx_path: &gxx,
        ar_path: pipeline::pick_archiver(&ar, &gcc_ar, &c, &cpp),
        c_flags: &c,
        cpp_flags: &cpp,
        include_dirs: &includes,
        verbose: params.verbose,
        jobs: crate::parallel::effective_jobs(params.jobs),
        compiler_cache: None,
    };
    let mcu = ctx.board.mcu.clone();
    let mut result = pipeline::run_sequential_build_with_optional_framework_libs(
        &compiler,
        &linker,
        ctx,
        params,
        &sources,
        &[],
        plan,
        Some(&env),
        Some(&archives),
        TargetArchitecture::Arm,
        "RP2040 Mbed",
        start,
    )
    .await?;
    if result.success && !params.compiledb_only && !params.clean_only {
        picotool.ensure_installed().await?;
        let elf = result.elf_path.as_deref().ok_or_else(|| {
            fbuild_core::FbuildError::BuildFailed(
                "Mbed link succeeded without an ELF artifact".to_string(),
            )
        })?;
        let uf2 = elf.with_extension("uf2");
        super::uf2::convert_elf_to_uf2(&picotool.executable(), elf, &uf2, &mcu).await?;
        result.firmware_path = Some(uf2);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_files_preserve_rp2040_abi_and_mbed_link_contract() {
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path();
        std::fs::create_dir(path.join("libs")).unwrap();
        for (name, text) in [
            (
                "cflags.txt",
                "-c -std=gnu11 -mcpu=cortex-m0plus -mthumb -fno-exceptions",
            ),
            (
                "cxxflags.txt",
                "-c -std=gnu++14 -mcpu=cortex-m0plus -mthumb -fno-rtti",
            ),
            (
                "ldflags.txt",
                "-DXIP_ENABLE=1 -mcpu=cortex-m0plus -mthumb -Wl,--wrap,main",
            ),
            ("linker_script.ld", "MEMORY {}"),
            ("libs/libmbed.a", "fixture"),
        ] {
            std::fs::write(path.join(name), text).unwrap();
        }
        let config = mcu_config(path, "RASPBERRY_PI_PICO").unwrap();
        assert!(
            config
                .compiler_flags
                .common
                .contains(&"-mcpu=cortex-m0plus".to_string())
        );
        assert!(config.compiler_flags.common.contains(&"-MMD".to_string()));
        assert!(
            config
                .compiler_flags
                .cxx
                .contains(&"-std=gnu++14".to_string())
        );
        assert!(config.linker_flags.contains(&"-Wl,--wrap,main".to_string()));
        assert!(
            !config
                .linker_flags
                .iter()
                .any(|flag| flag.contains("nano.specs")),
            "RP2040 Mbed must retain its published full-newlib runtime"
        );
        assert!(
            !config
                .linker_flags
                .iter()
                .any(|flag| flag.starts_with("-D"))
        );
        assert!(
            config
                .linker_libs
                .iter()
                .any(|flag| flag.ends_with("libmbed.a"))
        );
    }

    #[test]
    fn sdk_include_recipe_keeps_one_absolute_prefix() {
        let root = tempfile::TempDir::new().unwrap();
        let core = root.path().join("cores/arduino");
        std::fs::write(
            root.path().join("includes.txt"),
            "-iwithprefixbefore/mbed\n-iwithprefixbefore/mbed/targets/TARGET_RP2040\n",
        )
        .unwrap();
        let flags = sdk_include_flags(&core, root.path()).unwrap();
        assert_eq!(
            flags,
            vec![
                "-iprefix".to_string(),
                core.to_string_lossy().into_owned(),
                "-iwithprefixbefore/mbed".to_string(),
                "-iwithprefixbefore/mbed/targets/TARGET_RP2040".to_string()
            ]
        );
    }

    #[test]
    fn incomplete_variant_is_rejected_not_silently_replaced() {
        let root = tempfile::TempDir::new().unwrap();
        assert!(mcu_config(root.path(), "missing").is_err());
    }
}

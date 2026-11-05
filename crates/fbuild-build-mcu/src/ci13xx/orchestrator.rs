//! Compile the pinned vendor source manifest through the shared native engine.
use super::packages::VendorPackage;
use crate::{BuildOrchestrator, BuildParams, BuildResult};
use fbuild_core::{FbuildError, Platform, Result, SizeInfo};
use fbuild_packages::Package;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

type Properties = HashMap<String, String>;

fn fail(message: impl Into<String>) -> FbuildError {
    FbuildError::BuildFailed(message.into())
}

pub(super) fn validate(env: &Properties, mcu: &str) -> Result<()> {
    if mcu != "ci1306" || env.get("framework").map(String::as_str) != Some("arduino") {
        return Err(fail(
            "native CI13xx currently requires CI1306 and framework = arduino",
        ));
    }
    for key in env.keys() {
        if key == "platform_packages" || key.starts_with("board_build.") {
            return Err(fail(format!("CI13xx does not yet support {key}")));
        }
    }
    Ok(())
}

fn read_properties(path: &Path) -> Result<Properties> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            line.split_once('=')
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        })
        .collect())
}

pub(super) fn validate_board(board: &fbuild_config::BoardConfig, framework: &Path) -> Result<()> {
    let vendor = read_properties(&framework.join("boards.txt"))?;
    for (field, selected) in [
        ("f_cpu", &board.f_cpu),
        ("core", &board.core),
        ("variant", &board.variant),
    ] {
        let key = format!("ci1306.build.{field}");
        let expected = vendor
            .get(&key)
            .ok_or_else(|| fail(format!("missing vendor board property {key}")))?;
        let equal = if field == "f_cpu" {
            selected.trim_end_matches(&['L', 'l', 'U', 'u'][..])
                == expected.trim_end_matches(&['L', 'l', 'U', 'u'][..])
        } else {
            selected == expected
        };
        if !equal {
            return Err(fail(format!(
                "unsupported CI1306 {field} {selected}; pinned vendor recipe requires {expected}"
            )));
        }
    }
    Ok(())
}

fn expand(value: &str, props: &Properties) -> Result<String> {
    let mut result = value.to_owned();
    for _ in 0..20 {
        let Some(start) = result.find('{') else {
            return Ok(result);
        };
        let end = result[start..]
            .find('}')
            .ok_or_else(|| fail("invalid vendor property"))?
            + start;
        let key = &result[start + 1..end];
        let replacement = props
            .get(key)
            .ok_or_else(|| fail(format!("unresolved CI13xx property {key}")))?;
        result.replace_range(start..=end, replacement);
    }
    Err(fail("recursive vendor properties"))
}

fn flags(key: &str, props: &Properties) -> Result<Vec<String>> {
    let value = props
        .get(key)
        .ok_or_else(|| fail(format!("missing vendor property {key}")))?;
    Ok(fbuild_config::ini_parser::parse_flags(&expand(
        value, props,
    )?))
}

fn sources(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.map_err(|e| fail(e.to_string()))?;
        if entry.file_type().is_file()
            && matches!(
                entry.path().extension().and_then(|e| e.to_str()),
                Some("c" | "cpp" | "S" | "s")
            )
        {
            found.push(entry.into_path());
        }
    }
    found.sort();
    Ok(found)
}

async fn command(args: &[String], cwd: &Path) -> Result<String> {
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let output = fbuild_core::subprocess::run_command(&refs, Some(cwd), None, None).await?;
    if !output.success() {
        return Err(fail(format!(
            "CI13xx tool failed ({}): {}\n{}",
            output.exit_code, output.stdout, output.stderr
        )));
    }
    Ok(output.stdout)
}

struct Recipe {
    project: PathBuf,
    build: PathBuf,
    bin: PathBuf,
    framework: PathBuf,
    props: Properties,
    user_flags: Vec<String>,
    unflags: Vec<String>,
    board: fbuild_config::BoardConfig,
}

fn configure_recipe(
    params: &BuildParams,
    project: PathBuf,
    framework: PathBuf,
    toolchain: PathBuf,
    config: &fbuild_config::PlatformIOConfig,
    board: &fbuild_config::BoardConfig,
) -> Result<Recipe> {
    validate_board(board, &framework)?;
    std::fs::create_dir_all(&params.build_dir)?;
    let build = params.build_dir.canonicalize()?;
    let bin = toolchain.join("bin");
    let mut props = read_properties(&framework.join("platform.txt"))?;
    for (key, value) in read_properties(&framework.join("boards.txt"))? {
        if let Some(key) = key.strip_prefix("ci1306.build.") {
            props.insert(format!("build.{key}"), value);
        }
    }
    for (key, value) in [
        ("runtime.platform.path", framework.display().to_string()),
        (
            "runtime.tools.riscv-gcc.path",
            toolchain.display().to_string(),
        ),
        ("runtime.ide.version", "10819".into()),
        ("build.path", build.display().to_string()),
        ("build.project_name", "firmware".into()),
        (
            "build.variant.path",
            framework
                .join("variants")
                .join(&board.variant)
                .display()
                .to_string(),
        ),
    ] {
        props.insert(key.into(), value);
    }
    let host_libs = props
        .get("compiler.sdk.library.path.linux")
        .cloned()
        .ok_or_else(|| fail("missing cross-host library recipe"))?;
    props.insert("compiler.sdk.library.path".into(), host_libs);
    let user_flags: Vec<_> = config
        .get_build_flags(&params.env_name)?
        .into_iter()
        .chain(params.extra_build_flags.iter().cloned())
        .collect();
    let unflags = config.get_build_unflags(&params.env_name)?;
    Ok(Recipe {
        project,
        build,
        bin,
        framework,
        props,
        user_flags,
        unflags,
        board: board.clone(),
    })
}

struct SourcePlan {
    sources: Vec<PathBuf>,
    includes: Vec<PathBuf>,
    defines: Vec<String>,
    sdk: PathBuf,
}

fn source_plan(recipe: &Recipe) -> Result<SourcePlan> {
    let Recipe {
        framework,
        project,
        props,
        ..
    } = recipe;
    let mut defines = vec![
        "-DARDUINO=10819".into(),
        "-DARDUINO_ARCH_CI13XX".into(),
        format!("-DF_CPU={}", recipe.board.f_cpu),
    ];
    for key in [
        "build.extra_flags",
        "build.mic_flags",
        "build.clock_flags",
        "compiler.sdk.defines",
    ] {
        defines.extend(flags(key, props)?);
    }
    let core = framework.join("cores").join(&recipe.board.core);
    let variant = framework.join("variants").join(&recipe.board.variant);
    let sdk = framework.join("tools/sdk");
    let manifest = std::fs::read_to_string(sdk.join("source_file.prj"))?;
    let mut sdk_sources = Vec::new();
    let mut includes = vec![
        core.clone(),
        variant.clone(),
        sdk.join("include"),
        sdk.join("include/sdk"),
        project.join("include"),
    ];
    for line in manifest.lines().map(str::trim) {
        if let Some(path) = line.strip_prefix("source-file:") {
            sdk_sources.push(sdk.join("src").join(path.trim()));
        }
        if let Some(path) = line.strip_prefix("include-path:") {
            includes.push(sdk.join("include/sdk").join(path.trim()));
        }
    }
    if sdk_sources.len() != 138 {
        return Err(fail("pinned CI13xx SDK must supply 138 sources"));
    }
    let mut all_sources = sources(&core)?;
    all_sources.extend(sources(&variant)?);
    all_sources.extend(sdk_sources);
    Ok(SourcePlan {
        sources: all_sources,
        includes,
        defines,
        sdk,
    })
}

fn prepare_sketch(recipe: &Recipe, params: &BuildParams, plan: &mut SourcePlan) -> Result<()> {
    let config =
        fbuild_config::PlatformIOConfig::from_path(&recipe.project.join("platformio.ini"))?;
    let configured_src = config.get_src_dir(&params.env_name)?;
    let source_dir = recipe.project.join(
        params
            .src_dir
            .as_deref()
            .or(configured_src.as_deref())
            .unwrap_or("src"),
    );
    let explicit = params.src_dir.is_some() || configured_src.is_some();
    let source_dir = if source_dir.exists() || explicit {
        source_dir
    } else {
        recipe.project.clone()
    };
    let generated = recipe.build.join("sketch");
    std::fs::create_dir_all(&generated)?;
    let scanner = crate::source_scanner::SourceScanner::new(&source_dir, &generated);
    let includes: Vec<_> = plan.includes.iter().map(PathBuf::as_path).collect();
    let filter = config.get_source_filter(&params.env_name)?;
    plan.sources.extend(
        scanner.scan_sketch_sources_filtered_with_include_roots(filter.as_deref(), &includes)?,
    );
    Ok(())
}

fn library_flags(recipe: &Recipe, plan: &SourcePlan, key: &str) -> Result<Vec<String>> {
    let mut result = flags(key, &recipe.props)?;
    result.extend(plan.defines.iter().cloned());
    result.retain(|flag| !recipe.unflags.contains(flag));
    result.extend(recipe.user_flags.iter().cloned());
    Ok(result)
}

async fn resolve_libraries(
    recipe: &Recipe,
    params: &BuildParams,
    plan: &mut SourcePlan,
) -> Result<crate::pipeline::LibDeps> {
    let config =
        fbuild_config::PlatformIOConfig::from_path(&recipe.project.join("platformio.ini"))?;
    let dependencies = config.get_lib_deps(&params.env_name)?;
    let ignored = config.get_lib_ignore(&params.env_name)?;
    let c_flags = library_flags(recipe, plan, "compiler.c.flags")?;
    let cpp_flags = library_flags(recipe, plan, "compiler.cpp.flags")?;
    crate::pipeline::resolve_lib_deps(
        &dependencies,
        &ignored,
        &recipe.project,
        &recipe.build,
        &recipe.bin.join("riscv-nuclei-elf-gcc"),
        &recipe.bin.join("riscv-nuclei-elf-g++"),
        &recipe.bin.join("riscv-nuclei-elf-ar"),
        &recipe.bin.join("riscv-nuclei-elf-gcc-ar"),
        &c_flags,
        &cpp_flags,
        &mut plan.includes,
        params.verbose,
        None,
    )
    .await
}

async fn compile_sources(recipe: &Recipe, params: &BuildParams) -> Result<(Vec<PathBuf>, PathBuf)> {
    let Recipe {
        project,
        build,
        bin,
        props,
        user_flags,
        unflags,
        ..
    } = recipe;
    let gcc = bin.join("riscv-nuclei-elf-gcc");
    let gxx = bin.join("riscv-nuclei-elf-g++");
    let mut plan = source_plan(recipe)?;
    let libraries = resolve_libraries(recipe, params, &mut plan).await?;
    prepare_sketch(recipe, params, &mut plan)?;
    let mut records = Vec::new();
    let mut objects = Vec::new();
    for (index, source) in plan.sources.iter().enumerate() {
        let ext = source.extension().and_then(|s| s.to_str()).unwrap_or("");
        let (compiler, flag_key) = match ext {
            "cpp" | "cc" | "cxx" | "C" => (&gxx, "compiler.cpp.flags"),
            "S" | "s" => (&gcc, "compiler.S.flags"),
            _ => (&gcc, "compiler.c.flags"),
        };
        let mut compile_flags = flags(flag_key, props)?;
        if source.starts_with(plan.sdk.join("src")) {
            compile_flags.extend([
                "-DCI_ARDUINO_CORE=1".into(),
                "-DCI_CONFIG_FILE=\"user_config.h\"".into(),
            ]);
        }
        compile_flags.extend(plan.defines.iter().cloned());
        compile_flags.extend(plan.includes.iter().map(|p| format!("-I{}", p.display())));
        compile_flags.retain(|flag| !unflags.contains(flag));
        compile_flags.extend(user_flags.iter().cloned());
        if source.starts_with(plan.sdk.join("src")) {
            // The pinned vendor compile_sdk_sources.py intentionally compiles
            // SDK objects without LTO. Its external scheduler call must reach
            // ld --wrap, which creates the Arduino task. SDK LTO can inline the
            // real scheduler first and silently discard the entire sketch.
            compile_flags.retain(|flag| flag != "-flto" && !flag.starts_with("-flto="));
            compile_flags.push("-fno-lto".into());
        }
        let output = build.join("objects").join(format!("{index}.o"));
        let result = crate::compiler::compile_source(
            compiler,
            source,
            &output,
            &compile_flags,
            &[],
            build,
            "ci13xx",
            params.verbose,
            None,
            &[],
        )
        .await?;
        if !result.success {
            return Err(fail(format!(
                "CI13xx compile {}: {}\n{}",
                source.display(),
                result.stdout,
                result.stderr
            )));
        }
        // Native GNU commands, retained after successful engine execution;
        // unlike IDE databases this file is never rewritten to clang.
        let mut arguments = vec![compiler.display().to_string()];
        arguments.extend(compile_flags);
        arguments.extend([
            "-c".into(),
            source.display().to_string(),
            "-o".into(),
            output.display().to_string(),
        ]);
        records.push(serde_json::json!({"directory":project,"file":source,"arguments":arguments,"output":output}));
        objects.push(output);
    }
    let database = build.join("compile_commands.json");
    std::fs::write(
        &database,
        serde_json::to_vec_pretty(&records).map_err(|e| fail(e.to_string()))?,
    )?;
    let gate = fbuild_packages::library::library_compiler::job_gate(
        crate::parallel::effective_jobs(params.jobs),
    );
    objects.extend(libraries.compile(&gate).await?);
    Ok((objects, database))
}

async fn link_firmware(
    recipe: &Recipe,
    objects: &[PathBuf],
) -> Result<(PathBuf, PathBuf, SizeInfo)> {
    let Recipe {
        project,
        build,
        bin,
        props,
        user_flags,
        unflags,
        ..
    } = recipe;
    let gxx = bin.join("riscv-nuclei-elf-g++");
    let elf = build.join("firmware.elf");
    let mut link = vec![gxx.display().to_string()];
    let mut link_flags = flags("compiler.c.elf.flags", props)?;
    link_flags.retain(|flag| !unflags.contains(flag));
    link.extend(link_flags);
    link.extend(user_flags.iter().cloned());
    link.extend(flags("compiler.c.elf.wrap", props)?);
    link.extend(flags("compiler.c.elf.extra_flags", props)?);
    link.extend([
        "-o".into(),
        elf.display().to_string(),
        "-Wl,--start-group".into(),
    ]);
    link.extend(objects.iter().map(|p| p.display().to_string()));
    link.extend(flags("compiler.c.elf.libs", props)?);
    link.extend(["-Wl,--end-group".into(), "-Wl,-EL".into()]);
    command(&link, project).await?;
    std::fs::write(
        build.join("native_link_command.json"),
        serde_json::to_vec_pretty(&link).map_err(|e| fail(e.to_string()))?,
    )?;
    let firmware = build.join("firmware.user_code.bin");
    command(
        &[
            bin.join("riscv-nuclei-elf-objcopy").display().to_string(),
            "-O".into(),
            "binary".into(),
            elf.display().to_string(),
            firmware.display().to_string(),
        ],
        project,
    )
    .await?;
    let size = command(
        &[
            bin.join("riscv-nuclei-elf-size").display().to_string(),
            elf.display().to_string(),
        ],
        project,
    )
    .await?;
    let size_info = SizeInfo::parse(&size, recipe.board.max_flash, recipe.board.max_ram)
        .ok_or_else(|| fail(format!("invalid Nuclei size output: {size}")))?;
    Ok((elf, firmware, size_info))
}

pub struct Ci13xxOrchestrator;

#[async_trait::async_trait]
impl BuildOrchestrator for Ci13xxOrchestrator {
    fn platform(&self) -> Platform {
        Platform::Ci13xx
    }

    async fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let started = std::time::Instant::now();
        let project = params.project_dir.canonicalize()?;
        let config = fbuild_config::PlatformIOConfig::from_path(&project.join("platformio.ini"))?;
        let env = config.get_env_config(&params.env_name)?;
        let board = crate::resolution::ResolutionContext::new(&project, &params.env_name, &config)
            .resolve_board()?;
        validate(env, &board.mcu)?;
        if params.compiledb_only || params.clean_only || params.bloat_analysis {
            return Err(fail(
                "CI13xx initial native port does not support compiledb-only, clean-only or bloat mode",
            ));
        }
        if project.join("recursos").exists() {
            return Err(fail(
                "CI13xx custom resources require unimplemented resource-generation hooks",
            ));
        }
        let framework = VendorPackage::framework(&project)
            .ensure_installed()
            .await?;
        let toolchain = VendorPackage::toolchain(&project)?
            .ensure_installed()
            .await?;
        let recipe = configure_recipe(params, project, framework, toolchain, &config, &board)?;
        let (objects, database) = compile_sources(&recipe, params).await?;
        let (elf, firmware, size_info) = link_firmware(&recipe, &objects).await?;
        Ok(BuildResult {
            success: true,
            firmware_path: Some(firmware),
            elf_path: Some(elf),
            size_info: Some(size_info),
            symbol_map: None,
            build_time_secs: started.elapsed().as_secs_f64(),
            message: "Native CI1306 ELF/user-code build (combined firmware/deploy unsupported)"
                .into(),
            compile_database_path: Some(database),
            build_log: fbuild_core::BuildLog::new(),
        })
    }
}

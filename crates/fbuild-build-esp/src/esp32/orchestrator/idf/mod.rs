//! ESP-IDF owns the component build; fbuild orchestrates packages and artifacts.

mod artifacts;
mod packages;
mod project;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use fbuild_core::{FbuildError, Result};
use fbuild_packages::Toolchain;

pub(super) fn requested(config: &fbuild_config::PlatformIOConfig, env: &str) -> Result<bool> {
    let values = config.get_env_config(env)?;
    let frameworks =
        super::helpers::requested_frameworks(values.get("framework").map_or("", String::as_str));
    Ok(frameworks.espidf && !frameworks.arduino)
}

fn validate(mcu: &str, env: &HashMap<String, String>, params: &crate::BuildParams) -> Result<()> {
    if !packages::PackageSet::load()?
        .targets
        .iter()
        .any(|target| target == mcu)
    {
        return Err(FbuildError::ConfigError(format!(
            "framework = espidf is not yet supported for {mcu}: the ESP-IDF CMake/ninja adapter currently supports ESP32-S3 only (FastLED/fbuild#1664)",
        )));
    }
    if params.compiledb_only {
        return Err(FbuildError::ConfigError(
            "ESP-IDF compiledb-only mode is not yet supported".into(),
        ));
    }
    if env.get("framework").is_some_and(|frameworks| {
        frameworks
            .split(',')
            .any(|f| !f.trim().eq_ignore_ascii_case("espidf"))
    }) {
        return Err(FbuildError::ConfigError(
            "the ESP-IDF adapter supports framework=espidf only".into(),
        ));
    }
    // No silent Arduino platform pin substitution. Version selection will grow
    // with validated coherent IDF package sets, not independent compiler pins.
    if env.get("platform").is_some_and(|s| s != "espressif32")
        || env.contains_key("platform_packages")
    {
        return Err(FbuildError::ConfigError("ESP-IDF currently uses the pinned IDF 5.4 package set; platform version/URL and platform_packages overrides are not yet supported".into()));
    }
    for key in [
        "extra_scripts",
        "build_src_filter",
        "build_src_flags",
        "build_unflags",
        "custom_sdkconfig",
    ] {
        if env.get(key).is_some_and(|s| !s.trim().is_empty()) {
            return Err(FbuildError::ConfigError(format!(
                "{key} is not supported by the ESP-IDF adapter; use native component CMake/Kconfig settings"
            )));
        }
    }
    if params.symbol_analysis
        || params.bloat_analysis
        || params.shrink != crate::shrink::ShrinkMode::default()
    {
        return Err(FbuildError::ConfigError(
            "ESP-IDF symbol analysis, bloat and shrink modes are not yet supported".into(),
        ));
    }
    Ok(())
}

async fn command(
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    log: &mut fbuild_core::build_log::BuildLog,
) -> Result<String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let result = fbuild_core::subprocess::run_command(
        &args,
        Some(cwd),
        Some(&env),
        Some(Duration::from_secs(1800)),
    )
    .await?;
    for line in result.stdout.lines().chain(result.stderr.lines()) {
        log.push(line.to_string());
    }
    if !result.success() {
        return Err(FbuildError::BuildFailed(format!(
            "ESP-IDF command {} failed:\n{}\n{}",
            args.join(" "),
            result.stdout,
            result.stderr
        )));
    }
    Ok(result.stdout.trim().to_string())
}

pub(super) async fn build(
    params: &crate::BuildParams,
    config: &fbuild_config::PlatformIOConfig,
) -> Result<crate::BuildResult> {
    let start = Instant::now();
    let env = config.get_env_config(&params.env_name)?;
    let board =
        crate::resolution::ResolutionContext::new(&params.project_dir, &params.env_name, config)
            .resolve_board()?;
    validate(&board.mcu, env, params)?;
    let mut ctx = crate::pipeline::BuildContext::new(params).await?;
    if params.clean_only {
        return Ok(crate::BuildResult {
            success: true,
            firmware_path: None,
            elf_path: None,
            size_info: None,
            symbol_map: None,
            build_time_secs: start.elapsed().as_secs_f64(),
            message: format!("cleaned {}", params.env_name),
            compile_database_path: None,
            build_log: ctx.build_log,
        });
    }
    let build = ctx.build_dir.join("idf");
    std::fs::create_dir_all(&build)?;
    let caller_env = params
        .caller_path
        .as_ref()
        .map(|p| vec![("PATH".into(), p.clone())])
        .unwrap_or_default();
    let packages = packages::provision(
        &params.project_dir,
        &build,
        &board.mcu,
        &mut ctx.build_log,
        &caller_env,
    )
    .await?;
    ctx.build_log.push("ESP32 packages: requested=espidf, effective=espidf, integration=idf-cmake; ESP-IDF 5.4 / GCC 14.2");
    let root = project::source_root(&params.project_dir, &ctx.src_dir, &build)?;
    let mut flags = ctx.user_flags.clone();
    flags.extend(params.extra_build_flags.clone());
    let sdkconfig = project::sdkconfig(&params.project_dir, &build, &board.mcu, env, &flags)?;
    let environment = environment(&packages, params)?;
    let python = packages.python.display();
    let mut configure = vec![
        packages.cmake.to_string_lossy().into_owned(),
        "-S".into(),
        root.to_string_lossy().into_owned(),
        "-B".into(),
        build.to_string_lossy().into_owned(),
        "-G".into(),
        "Ninja".into(),
        format!("-DIDF_TARGET={}", board.mcu),
        format!("-DPYTHON={python}"),
        "-DPYTHON_DEPS_CHECKED=1".into(),
        "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON".into(),
        format!("-DSDKCONFIG={}", sdkconfig.display()),
        format!("-DCMAKE_MAKE_PROGRAM={}", packages.ninja.display()),
    ];
    configure.extend(cmake_options(env, &flags)?);
    command(
        &configure,
        &params.project_dir,
        &environment,
        &mut ctx.build_log,
    )
    .await?;
    let compile = vec![
        packages.cmake.to_string_lossy().into_owned(),
        "--build".into(),
        build.to_string_lossy().into_owned(),
        "--parallel".into(),
        params.jobs.unwrap_or(2).max(1).to_string(),
    ];
    command(
        &compile,
        &params.project_dir,
        &environment,
        &mut ctx.build_log,
    )
    .await?;
    let linked = artifacts::export(&ctx, &build, &packages.toolchain).await?;
    crate::pipeline::handle_link_result(&linked, &mut ctx.build_log, None, params.verbose);
    Ok(crate::pipeline::assemble_build_result(
        linked,
        start.elapsed().as_secs_f64(),
        "ESP32-S3 (ESP-IDF)",
        &params.env_name,
        Some(ctx.build_dir.join("compile_commands.json")),
        ctx.build_log,
    ))
}

fn environment(
    packages: &packages::IdfPackages,
    params: &crate::BuildParams,
) -> Result<Vec<(String, String)>> {
    let python_bin = packages
        .python
        .parent()
        .ok_or_else(|| FbuildError::BuildFailed("invalid IDF Python path".into()))?;
    let mut paths = vec![python_bin.to_path_buf(), packages.toolchain.get_bin_dir()];
    let caller_path = params
        .caller_path
        .clone()
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
    paths.extend(std::env::split_paths(&caller_path));
    let path = std::env::join_paths(paths).map_err(|e| FbuildError::ConfigError(e.to_string()))?;
    Ok(vec![
        (
            "IDF_PATH".into(),
            packages.framework.to_string_lossy().into_owned(),
        ),
        (
            "IDF_PYTHON_ENV_PATH".into(),
            packages.python_env.to_string_lossy().into_owned(),
        ),
        ("PATH".into(), path.to_string_lossy().into_owned()),
    ])
}

fn cmake_options(env: &HashMap<String, String>, flags: &[String]) -> Result<Vec<String>> {
    let mut args = fbuild_core::shell_split::split(
        env.get("board_build.cmake_extra_args")
            .map_or("", String::as_str),
    );
    for arg in &args {
        let name = arg
            .strip_prefix("-D")
            .unwrap_or("")
            .split(['=', ':'])
            .next()
            .unwrap_or("");
        if name.is_empty()
            || [
                "CMAKE_C_COMPILER",
                "CMAKE_CXX_COMPILER",
                "CMAKE_ASM_COMPILER",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix))
            || [
                "IDF_TARGET",
                "IDF_PATH",
                "SDKCONFIG",
                "PYTHON",
                "CMAKE_TOOLCHAIN_FILE",
                "CMAKE_MAKE_PROGRAM",
                "CMAKE_AR",
                "CMAKE_LINKER",
            ]
            .contains(&name)
        {
            return Err(FbuildError::ConfigError(format!(
                "unsupported ESP-IDF cmake_extra_args argument: {arg}"
            )));
        }
    }
    let flags = flags
        .iter()
        .filter(|f| !f.starts_with("-DCONFIG_"))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    for variable in ["CMAKE_C_FLAGS", "CMAKE_CXX_FLAGS"] {
        let explicit = args.iter().any(|arg| {
            arg.strip_prefix("-D")
                .is_some_and(|arg| arg.split(['=', ':']).next() == Some(variable))
        });
        if explicit && !flags.is_empty() {
            return Err(FbuildError::ConfigError(format!(
                "use either build_flags or cmake_extra_args {variable}, not both"
            )));
        }
        if !explicit {
            // Reset a previous fbuild overlay when build_flags is removed.
            args.push(format!("-D{variable}={flags}"));
        }
    }
    Ok(args)
}

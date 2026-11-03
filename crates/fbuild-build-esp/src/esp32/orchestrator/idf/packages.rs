//! Resolve IDF packages using the same registry, checksums and cache as Arduino.

use std::path::{Path, PathBuf};

use fbuild_core::platformio_package::{PackageKind, ResolvedPayload, parse_package_spec};
use fbuild_core::{FbuildError, Result};
use fbuild_packages::{CacheSubdir, Package, PackageBase};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct PackageSet {
    pub targets: Vec<String>,
    pub framework: String,
    pub toolchain: String,
    pub constraints: String,
    pub python: String,
    pub cmake: String,
    pub ninja: String,
}

impl PackageSet {
    pub fn load() -> Result<Self> {
        serde_json::from_str(include_str!("packages.json"))
            .map_err(|e| FbuildError::PackageError(e.to_string()))
    }
}

pub(super) struct IdfPackages {
    pub framework: PathBuf,
    pub toolchain: fbuild_packages::toolchain::Esp32Toolchain,
    pub python: PathBuf,
    pub python_env: PathBuf,
    pub cmake: PathBuf,
    pub ninja: PathBuf,
}

async fn resolve(project: &Path, spec: &str) -> Result<ResolvedPayload> {
    let parsed = parse_package_spec(spec).map_err(|e| FbuildError::PackageError(e.to_string()))?;
    let registry = parsed.registry().ok_or_else(|| {
        FbuildError::PackageError(format!("IDF package must be a registry package: {spec}"))
    })?;
    let host = fbuild_core::platformio_package::host_system(fbuild_core::platform::host::current())
        .ok_or_else(|| FbuildError::PackageError("unsupported ESP-IDF host".into()))?;
    fbuild_packages::platformio_registry::RegistryClient::default()
        .resolve_cached(
            registry,
            PackageKind::Tool,
            host,
            &fbuild_packages::Cache::new(project).platforms_dir(),
            true,
        )
        .await
        .map_err(|e| FbuildError::PackageError(e.to_string()))?
        .ok_or_else(|| FbuildError::PackageError(format!("IDF package unavailable: {spec}")))
}

pub(super) async fn provision(
    project: &Path,
    build: &Path,
    mcu: &str,
    log: &mut fbuild_core::build_log::BuildLog,
    env: &[(String, String)],
) -> Result<IdfPackages> {
    let pins = PackageSet::load()?;
    let prefix = super::super::super::mcu_config::get_mcu_config(mcu)?.toolchain_prefix();
    let framework = resolve(project, &pins.framework).await?;
    let toolchain = resolve(project, &pins.toolchain).await?;
    let base = PackageBase::new(
        &framework.name,
        &framework.version,
        &framework.url,
        &framework.cache_identity(),
        Some(&framework.sha256),
        CacheSubdir::Platforms,
        project,
    );
    let framework_path = base
        .staged_install(|root| {
            if root.join("tools/cmake/project.cmake").is_file() {
                Ok(())
            } else {
                Err(FbuildError::PackageError(
                    "invalid framework-espidf layout".into(),
                ))
            }
        })
        .await?;
    let toolchain = fbuild_packages::toolchain::Esp32Toolchain::from_registry_payload(
        project, &toolchain, &prefix,
    );
    toolchain.ensure_installed().await?;
    let (python_env, bin, python) =
        prepare_python(project, build, &framework_path, &pins, env, log).await?;
    Ok(IdfPackages {
        framework: framework_path,
        toolchain,
        python,
        python_env,
        cmake: bin.join(if cfg!(windows) { "cmake.exe" } else { "cmake" }),
        ninja: bin.join(if cfg!(windows) { "ninja.exe" } else { "ninja" }),
    })
}

async fn prepare_python(
    project: &Path,
    build: &Path,
    framework_path: &Path,
    pins: &PackageSet,
    env: &[(String, String)],
    log: &mut fbuild_core::build_log::BuildLog,
) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let requirements = build.join("python-requirements.txt");
    super::project::write_changed(
        &requirements,
        &format!(
            "-r {}\n-c {}\ncmake=={}\nninja=={}\n",
            framework_path
                .join("tools/requirements/requirements.core.txt")
                .display(),
            pins.constraints,
            pins.cmake,
            pins.ninja,
        ),
    )?;
    let python_env = build.join("python-env");
    if !python_env
        .join(if cfg!(windows) {
            "Scripts/python.exe"
        } else {
            "bin/python"
        })
        .is_file()
    {
        super::command(
            &[
                "uv".into(),
                "venv".into(),
                "--allow-existing".into(),
                "--python".into(),
                pins.python.clone(),
                python_env.to_string_lossy().into_owned(),
            ],
            project,
            env,
            log,
        )
        .await?;
    }
    let bin = python_env.join(if cfg!(windows) { "Scripts" } else { "bin" });
    let python = bin.join(if cfg!(windows) {
        "python.exe"
    } else {
        "python"
    });
    let installed = python_env.join("fbuild-requirements.txt");
    let required = std::fs::read_to_string(&requirements)?;
    if std::fs::read_to_string(&installed).ok().as_deref() != Some(&required) {
        super::command(
            &[
                "uv".into(),
                "pip".into(),
                "install".into(),
                "--python".into(),
                python.to_string_lossy().into_owned(),
                "-r".into(),
                requirements.to_string_lossy().into_owned(),
            ],
            project,
            env,
            log,
        )
        .await?;
        std::fs::write(&installed, &required)?;
    }
    Ok((python_env, bin, python))
}

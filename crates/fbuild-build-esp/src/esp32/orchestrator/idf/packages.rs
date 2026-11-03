//! Resolve IDF packages using the same registry, checksums and cache as Arduino.

use std::path::Path;

use fbuild_core::path::NormalizedPath;

use fbuild_core::platform::executable;
use fbuild_core::platformio_package::{PackageKind, ResolvedPayload, parse_package_spec};
use fbuild_core::{FbuildError, Result};
use fbuild_packages::{CacheSubdir, Package, PackageBase};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct PackageSet {
    pub targets: Vec<String>,
    pub framework: String,
    pub toolchains: Vec<String>,
    pub constraints: String,
    pub python: String,
    pub cmake: String,
    pub ninja: String,
    #[serde(default)]
    pub revisions: std::collections::HashMap<String, std::collections::HashMap<String, Revision>>,
}

impl PackageSet {
    pub fn load(mcu: &str) -> Result<Self> {
        let sets: Vec<Self> = serde_json::from_str(include_str!("packages.json"))
            .map_err(|e| FbuildError::PackageError(e.to_string()))?;
        sets.into_iter()
            .find(|set| set.targets.iter().any(|target| target == mcu))
            .ok_or_else(|| {
                FbuildError::ConfigError(format!(
                    "framework = espidf is not yet supported for {mcu}"
                ))
            })
    }
}

#[derive(Deserialize)]
pub(super) struct Revision {
    pub defaults: String,
    pub selector: String,
    pub enabled: bool,
}

pub(super) fn revision(board: &fbuild_config::BoardConfig) -> Result<Option<Revision>> {
    let mut pins = PackageSet::load(&board.mcu)?;
    let Some(mut variants) = pins.revisions.remove(&board.mcu) else {
        return Ok(None);
    };
    variants
        .remove(board.sdk_variant())
        .map(Some)
        .ok_or_else(|| {
            FbuildError::ConfigError(format!(
                "ESP-IDF: unsupported {} silicon variant {}",
                board.mcu,
                board.sdk_variant()
            ))
        })
}

pub(super) struct IdfPackages {
    pub framework: NormalizedPath,
    pub toolchain: fbuild_packages::toolchain::Esp32Toolchain,
    pub python: NormalizedPath,
    pub python_env: NormalizedPath,
    pub cmake: NormalizedPath,
    pub ninja: NormalizedPath,
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
    let pins = PackageSet::load(mcu)?;
    let prefix = super::super::super::mcu_config::get_mcu_config(mcu)?.toolchain_prefix();
    let framework = resolve(project, &pins.framework).await?;
    let mut tools = Vec::new();
    for spec in &pins.toolchains {
        tools.push(resolve(project, spec).await?);
    }
    let toolchain = tools
        .first()
        .ok_or_else(|| FbuildError::PackageError("IDF package set has no compiler".into()))?;
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
        project, toolchain, &prefix,
    );
    toolchain.ensure_installed().await?;
    let (python_env, bin, python) =
        prepare_python(project, build, &framework_path, &pins, env, log).await?;
    Ok(IdfPackages {
        framework: framework_path.into(),
        toolchain,
        python,
        python_env,
        cmake: bin.join(executable::native_name("cmake")),
        ninja: bin.join(executable::native_name("ninja")),
    })
}

async fn prepare_python(
    project: &Path,
    build: &Path,
    framework_path: &Path,
    pins: &PackageSet,
    env: &[(String, String)],
    log: &mut fbuild_core::build_log::BuildLog,
) -> Result<(NormalizedPath, NormalizedPath, NormalizedPath)> {
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
    let python_env = NormalizedPath::new(build).join("python-env");
    let python = python_env.join(executable::name("bin/python", "Scripts/python.exe"));
    let installed = python_env.join("fbuild-requirements.txt");
    let recreated = !python.is_file();
    if recreated {
        // A failed recreation/install must never leave an old valid-looking stamp.
        if installed.is_file() {
            std::fs::remove_file(&installed)?;
        }
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
    let bin =
        NormalizedPath::new(python.parent().ok_or_else(|| {
            FbuildError::PackageError("invalid IDF Python environment path".into())
        })?);
    let required = std::fs::read_to_string(&requirements)?;
    if recreated || std::fs::read_to_string(&installed).ok().as_deref() != Some(&required) {
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

#[cfg(test)]
mod tests {
    #[tokio::test]
    #[ignore = "requires uv, Python 3.11 and network for CMake/ninja packages"]
    async fn recreated_python_env_reinstalls_matching_requirements() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path();
        let framework = project.join("framework");
        let requirements = framework.join("tools/requirements");
        std::fs::create_dir_all(&requirements).unwrap();
        std::fs::write(requirements.join("requirements.core.txt"), "").unwrap();
        let constraints = project.join("constraints.txt");
        std::fs::write(&constraints, "").unwrap();
        let build = project.join("build");
        std::fs::create_dir(&build).unwrap();
        let mut pins = super::PackageSet::load("esp32s3").unwrap();
        pins.constraints = constraints.to_string_lossy().into_owned();
        let mut log = fbuild_core::build_log::BuildLog::default();
        let (env, _, _) = super::prepare_python(project, &build, &framework, &pins, &[], &mut log)
            .await
            .unwrap();
        let marker = std::fs::read_to_string(env.join("fbuild-requirements.txt")).unwrap();
        std::fs::remove_dir_all(&env).unwrap();
        std::fs::create_dir(&env).unwrap();
        std::fs::write(env.join("fbuild-requirements.txt"), &marker).unwrap();
        let (_, bin, _) = super::prepare_python(project, &build, &framework, &pins, &[], &mut log)
            .await
            .unwrap();
        for tool in ["cmake", "ninja"] {
            let executable = bin.join(fbuild_core::platform::executable::native_name(tool));
            assert!(
                executable.is_file(),
                "missing {} after venv recreation",
                executable.display()
            );
        }
    }
}

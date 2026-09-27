//! Build orchestration facade for all supported platforms.
//!
//! FastLED/fbuild#1008 compile-parallelism split: the shared engine lives in
//! `fbuild-build-engine`; each platform family lives in its own crate; this
//! facade re-exports everything at its original paths and owns the one piece of
//! code that must see both the `PlatformSupport` trait and every platform impl
//! — the `get_platform_support()` factory. Consumers (`fbuild-cli`,
//! `fbuild-daemon`, …) keep depending on `fbuild-build` with unchanged paths:
//! `fbuild_build::pipeline::…`, `fbuild_build::esp32::…`,
//! `fbuild_build::PlatformSupport`, `fbuild_build::get_platform_support`.

// Re-export the entire engine surface at the facade root so every existing
// `fbuild_build::<engine_item>` path (and every in-crate `crate::<engine_item>`
// reference from the platform modules below) keeps resolving unchanged.
pub use fbuild_build_engine::*;

// `compile_many` is the multi-sketch driver: it dispatches per-platform via
// `get_orchestrator()`, so it lives in the facade (not the engine) alongside
// the platform factory. It references engine modules as `crate::<mod>`, which
// resolve through the `pub use fbuild_build_engine::*` re-export above.
pub mod compile_many;

// Platform orchestrators, now in per-family crates that compile in parallel on
// top of the engine (FastLED/fbuild#1008 A2). Re-exported here so every
// existing `fbuild_build::esp32::…` / `fbuild_build::teensy::…` path — and the
// `get_platform_support()` factory below — keeps resolving unchanged.
pub use fbuild_build_arm::{
    apollo3, generic_arm, nrf52, nxplpc, renesas, rp2040, sam, silabs, stm32, teensy,
};
pub use fbuild_build_esp::{esp32, esp8266};
pub use fbuild_build_mcu::{avr, ch32v};

use std::path::Path;

use fbuild_core::{Platform, Result};

/// Look up platform support for a given platform.
///
/// This is the ONE piece of code that must see both the [`PlatformSupport`]
/// trait (from the engine) and every platform implementation, so it lives in
/// the facade. Returns `Err` for platforms without a native orchestrator.
pub fn get_platform_support(platform: Platform) -> Result<Box<dyn PlatformSupport>> {
    match platform {
        Platform::Apollo3 => Ok(Box::new(apollo3::Apollo3PlatformSupport)),
        Platform::AtmelAvr | Platform::AtmelMegaAvr => Ok(Box::new(avr::AvrPlatformSupport)),
        Platform::Espressif32 => Ok(Box::new(esp32::Esp32PlatformSupport)),
        Platform::Espressif8266 => Ok(Box::new(esp8266::Esp8266PlatformSupport)),
        Platform::Teensy => Ok(Box::new(teensy::TeensyPlatformSupport)),
        Platform::Ststm32 => Ok(Box::new(stm32::Stm32PlatformSupport)),
        Platform::RaspberryPi => Ok(Box::new(rp2040::Rp2040PlatformSupport)),
        Platform::NordicNrf52 => Ok(Box::new(nrf52::Nrf52PlatformSupport)),
        Platform::NxpLpc => Ok(Box::new(nxplpc::NxpLpcPlatformSupport)),
        Platform::AtmelSam => Ok(Box::new(sam::SamPlatformSupport)),
        Platform::RenesasRa => Ok(Box::new(renesas::RenesasPlatformSupport)),
        Platform::SiliconLabs => Ok(Box::new(silabs::SilabsPlatformSupport)),
        Platform::Ch32v => Ok(Box::new(ch32v::Ch32vPlatformSupport)),
        _ => Err(fbuild_core::FbuildError::BuildFailed(format!(
            "native orchestrator for {:?} not yet implemented — use --platformio flag for this platform",
            platform
        ))),
    }
}

/// Select the appropriate orchestrator for a platform.
pub fn get_orchestrator(platform: Platform) -> Result<Box<dyn BuildOrchestrator>> {
    get_platform_support(platform).map(|s| s.create_orchestrator())
}

/// Resolve an env's board and platform, then provision everything its build
/// downloads — platform packages, toolchains, framework, tools and `lib_deps`
/// — without compiling. The engine behind `fbuild install` and the daemon's
/// `POST /api/install-deps` (FastLED/fbuild#1433).
pub async fn provision_env(
    project_dir: &Path,
    env_name: &str,
    mode: provision::ProvisionMode,
) -> Result<provision::ProvisionReport> {
    let config = fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))?;
    let env_config = config.get_env_config(env_name)?;
    let board =
        resolution::ResolutionContext::new(project_dir, env_name, &config).resolve_board()?;
    let platform = env_config
        .get("platform")
        .and_then(|value| Platform::from_platform_str(value))
        .or_else(|| board.platform())
        .ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError(format!(
                "could not determine the platform for environment '{env_name}'"
            ))
        })?;
    if !mode.fetches() && platform != Platform::Espressif32 && has_registry_version_pin(env_config)?
    {
        return Err(fbuild_core::FbuildError::PackageError(
            "offline install check/dry-run cannot resolve a pinned PlatformIO platform or package without fetching its manifest; run `fbuild install`".into(),
        ));
    }
    let support = get_platform_support(platform)?;
    let inputs = provision::ProvisionInputs {
        project_dir,
        env_name,
        env_config,
        board: &board,
    };

    let pinned_platform = (platform != Platform::Espressif32)
        .then(|| env_config.get("platform"))
        .flatten()
        .map(|value| fbuild_core::platformio_package::parse_package_spec(value))
        .transpose()
        .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?
        .and_then(|spec| {
            spec.registry()
                .filter(|registry| registry.requirement.is_some())
                .cloned()
        });
    let preinstalled_platform_identity = if let Some(registry) = &pinned_platform {
        let host = registry_host_system()?;
        let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
        let client = fbuild_packages::platformio_registry::RegistryClient::default();
        client
            .resolve_cached(
                registry,
                fbuild_core::platformio_package::PackageKind::Platform,
                host,
                &cache_root,
                false,
            )
            .await
            .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?
            .and_then(|payload| {
                platform_base_for_payload(project_dir, &payload)
                    .is_cached()
                    .then(|| payload.cache_identity())
            })
    } else {
        None
    };
    let mut packages = support.provision(&inputs, mode).await?;
    if let Some(registry) = &pinned_platform {
        let host = registry_host_system()?;
        let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
        let client = fbuild_packages::platformio_registry::RegistryClient::default();
        let payload = client
            .resolve_cached(
                registry,
                fbuild_core::platformio_package::PackageKind::Platform,
                host,
                &cache_root,
                false,
            )
            .await
            .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?
            .ok_or_else(|| {
                fbuild_core::FbuildError::PackageError(format!(
                    "pinned PlatformIO platform `{}` was not resolved during installation",
                    registry.name
                ))
            })?;
        let base = platform_base_for_payload(project_dir, &payload);
        if !base.is_cached() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "pinned PlatformIO platform `{}` was not installed",
                registry.name
            )));
        }
        if !packages.iter().any(|package| {
            package.kind == provision::PackageKind::Platform && package.name == payload.name
        }) {
            let info = base.get_info();
            packages.insert(
                0,
                provision::ProvisionedPackage {
                    kind: provision::PackageKind::Platform,
                    name: info.name,
                    version: info.version,
                    url: info.url,
                    sha256: info.checksum,
                    status: if preinstalled_platform_identity.as_deref()
                        == Some(payload.cache_identity().as_str())
                    {
                        provision::ProvisionStatus::Present
                    } else {
                        provision::ProvisionStatus::Fetched
                    },
                    bytes: info.installed_bytes,
                    duration_ms: 0,
                    install_path: Some(info.install_path.display().to_string()),
                    error: None,
                },
            );
        }
    }
    let lib_deps = support
        .downloadable_lib_deps(&inputs, config.get_lib_deps(env_name)?)
        .await?;
    let lib_ignore = config.get_lib_ignore(env_name)?;
    // `fbuild build` downloads lib_deps into the release build dir's `libs/`.
    let libs_dir = fbuild_paths::BuildLayout::new(
        project_dir.to_path_buf(),
        env_name.to_string(),
        fbuild_core::BuildProfile::Release,
    )
    .resolve()
    .join("libs");
    packages.extend(
        provision::provision_lib_deps(project_dir, &lib_deps, &lib_ignore, &libs_dir, mode).await,
    );

    Ok(provision::ProvisionReport {
        env: env_name.to_string(),
        platform: format!("{platform:?}"),
        packages,
    })
}

fn registry_host_system() -> Result<&'static str> {
    fbuild_core::platformio_package::host_system(fbuild_core::platform::host::current())
        .ok_or_else(|| fbuild_core::FbuildError::PackageError("unsupported PlatformIO host".into()))
}

fn platform_base_for_payload(
    project_dir: &Path,
    payload: &fbuild_core::platformio_package::ResolvedPayload,
) -> fbuild_packages::PackageBase {
    fbuild_packages::PackageBase::new(
        &payload.name,
        &payload.version,
        &payload.url,
        &payload.cache_identity(),
        Some(&payload.sha256),
        fbuild_packages::CacheSubdir::Platforms,
        project_dir,
    )
}

fn has_registry_version_pin(env: &std::collections::HashMap<String, String>) -> Result<bool> {
    use fbuild_core::platformio_package::parse_package_spec;
    let pinned_platform = env
        .get("platform")
        .map(|value| parse_package_spec(value))
        .transpose()
        .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?
        .is_some_and(|spec| {
            spec.registry()
                .is_some_and(|registry| registry.requirement.is_some())
        });
    if pinned_platform {
        return Ok(true);
    }
    for line in env
        .get("platform_packages")
        .into_iter()
        .flat_map(|raw| raw.lines())
    {
        let entry = line.trim().trim_end_matches([',', ';']).trim();
        if entry.is_empty() {
            continue;
        }
        let spec = parse_package_spec(entry)
            .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?;
        if spec.registry().is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(ini: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("platformio.ini"), ini).unwrap();
        dir
    }

    #[tokio::test]
    async fn provision_env_rejects_an_unknown_environment() {
        let dir = project("[env:teensy41]\nplatform = teensy\nboard = teensy41\n");
        let error = provision_env(dir.path(), "nope", provision::ProvisionMode::DryRun)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("nope"), "{error}");
    }

    #[tokio::test]
    async fn provision_env_dry_run_lists_packages_without_fetching() {
        let dir = project("[env:teensy41]\nplatform = teensy\nboard = teensy41\n");
        let report = provision_env(dir.path(), "teensy41", provision::ProvisionMode::DryRun)
            .await
            .unwrap();
        assert_eq!(report.env, "teensy41");
        let kinds: Vec<_> = report.packages.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![
                provision::PackageKind::Toolchain,
                provision::PackageKind::Framework
            ]
        );
        for package in &report.packages {
            assert!(
                matches!(
                    package.status,
                    provision::ProvisionStatus::Present | provision::ProvisionStatus::WouldFetch
                ),
                "a dry run must not fetch: {package:?}"
            );
        }
    }

    #[tokio::test]
    async fn pinned_platform_and_package_dry_run_fail_before_registry_fetch() {
        for ini in [
            "[env:teensy41]\nplatform = teensy@5.1.0\nboard = teensy41\n",
            "[env:teensy41]\nplatform = teensy\nboard = teensy41\nplatform_packages = framework-arduinoteensy@1.159.0\n",
        ] {
            let dir = project(ini);
            for mode in [
                provision::ProvisionMode::DryRun,
                provision::ProvisionMode::Check,
            ] {
                let error = provision_env(dir.path(), "teensy41", mode)
                    .await
                    .unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("cannot resolve a pinned PlatformIO")
                );
                assert!(!dir.path().join(fbuild_paths::FBUILD_DIR_NAME).exists());
            }
        }
    }

    #[test]
    fn test_get_orchestrator_atmelmegaavr() {
        let orch = get_orchestrator(Platform::AtmelMegaAvr).unwrap();
        assert_eq!(orch.platform(), Platform::AtmelAvr);
    }

    #[test]
    fn test_get_orchestrator_esp8266() {
        let orch = get_orchestrator(Platform::Espressif8266).unwrap();
        assert_eq!(orch.platform(), Platform::Espressif8266);
    }
}

//! ESP32 platform, framework, SDK and esptool package selection.

use super::*;

/// The pioarduino platform package. Honors
/// `platform_packages = platform-espressif32@<URL>#<sha>` (FastLED/fbuild#672),
/// then `platform = <release archive URL>` (FastLED/fbuild#1432): the pin
/// replaces the const-pinned default and gets its own cache subdir via
/// `PackageBase::with_override`.
pub(super) async fn pioarduino_platform(
    project_dir: &Path,
    env_config: Option<&HashMap<String, String>>,
    fetch: bool,
) -> Result<Option<fbuild_packages::library::Esp32Platform>> {
    let platform_ovr = env_config.and_then(|env| {
        crate::package_override::resolve_platform_override(env, "platform-espressif32")
    });
    if let Some(override_package) = platform_ovr {
        return Ok(Some(
            fbuild_packages::library::Esp32Platform::with_override(project_dir, override_package),
        ));
    }
    if let Some(pin) = env_config
        .and_then(|env| env.get("platform"))
        .map(String::as_str)
        .map(str::trim)
        .filter(|pin| pin.contains('@'))
    {
        let spec = parse_package_spec(pin)
            .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?;
        let PackageSource::Registry(registry) = spec.source else {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "unsupported ESP32 platform source `{pin}`"
            )));
        };
        if registry.name != "espressif32" {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "unsupported ESP32 platform alias `{pin}`"
            )));
        }
        let host =
            fbuild_core::platformio_package::host_system(fbuild_core::platform::host::current())
                .ok_or_else(|| {
                    fbuild_core::FbuildError::PackageError("unsupported PlatformIO host".into())
                })?;
        let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
        let Some(payload) = fbuild_packages::platformio_registry::RegistryClient::default()
            .resolve_cached(
                &registry,
                RegistryPackageKind::Platform,
                host,
                &cache_root,
                fetch,
            )
            .await
            .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?
        else {
            return Ok(None);
        };
        tracing::info!(
            "resolved requested ESP32 platform {}@{}: {} (sha256 {})",
            payload.name,
            payload.version,
            payload.url,
            payload.sha256
        );
        return Ok(Some(
            fbuild_packages::library::Esp32Platform::with_override(
                project_dir,
                fbuild_config::PackageOverride {
                    url: payload.url,
                    version: payload.version,
                    checksum: Some(payload.sha256),
                },
            ),
        ));
    }
    warn_unhonored_platform_pin(env_config);
    Ok(Some(fbuild_packages::library::Esp32Platform::new(
        project_dir,
    )))
}

/// The Arduino framework package. Override precedence (FastLED/fbuild#672):
///   1. `platform_packages = framework-arduinoespressif32@<URL>#<sha>` wins
///      outright — consumer-supplied URL replaces the platform.json-derived
///      URL and gets its own cache subdir.
///   2. Otherwise, derive the URL from platform.json.
///   3. Otherwise (very old / missing platform.json), fall back to the
///      legacy hardcoded URL via `Esp32Framework::new`.
pub(super) fn pioarduino_framework(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mcu: &str,
    env_config: Option<&HashMap<String, String>>,
) -> fbuild_packages::library::Esp32Framework {
    let framework_ovr = env_config.and_then(|env| {
        crate::package_override::resolve_override(env, "framework-arduinoespressif32")
    });
    match framework_ovr {
        Some(o) => fbuild_packages::library::Esp32Framework::with_override(project_dir, o),
        None => match platform.get_package_url("framework-arduinoespressif32") {
            Ok(url) => {
                tracing::info!("resolved framework URL from platform.json");
                fbuild_packages::library::Esp32Framework::from_url(project_dir, &url)
            }
            Err(e) => {
                tracing::warn!("could not resolve framework URL, using legacy: {}", e);
                fbuild_packages::library::Esp32Framework::new(project_dir, mcu)
            }
        },
    }
}

/// Resolve a registry framework requirement declared by a pinned PlatformIO
/// platform instead of passing its version string to the URL downloader.
pub(super) async fn resolve_framework(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mcu: &str,
    env_config: Option<&HashMap<String, String>>,
    fetch: bool,
) -> Result<Option<fbuild_packages::library::Esp32Framework>> {
    if let Some(override_package) = env_config.and_then(|env| {
        crate::package_override::resolve_override(env, "framework-arduinoespressif32")
    }) {
        return Ok(Some(
            fbuild_packages::library::Esp32Framework::with_override(project_dir, override_package),
        ));
    }
    let requirement = platform.get_package_requirement("framework-arduinoespressif32")?;
    if let Some(registry) = requirement.spec.registry() {
        let host =
            fbuild_core::platformio_package::host_system(fbuild_core::platform::host::current())
                .ok_or_else(|| {
                    fbuild_core::FbuildError::PackageError("unsupported PlatformIO host".into())
                })?;
        let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
        let Some(payload) = fbuild_packages::platformio_registry::RegistryClient::default()
            .resolve_cached(
                registry,
                RegistryPackageKind::Framework,
                host,
                &cache_root,
                fetch,
            )
            .await
            .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?
        else {
            return Ok(None);
        };
        tracing::info!(
            "resolved ESP32 framework {}@{}: {} (sha256 {})",
            payload.name,
            payload.version,
            payload.url,
            payload.sha256
        );
        return Ok(Some(
            fbuild_packages::library::Esp32Framework::with_override(
                project_dir,
                fbuild_config::PackageOverride {
                    url: payload.url,
                    version: payload.version,
                    checksum: Some(payload.sha256),
                },
            ),
        ));
    }
    Ok(Some(pioarduino_framework(
        platform,
        project_dir,
        mcu,
        env_config,
    )))
}

/// URLs of the split SDK libs package (pioarduino 3.3.7+) and, for MCUs that
/// ship one, the MCU skeleton libs (e.g. ESP32-C2, ESP32-C61).
pub(super) fn sdk_libs_urls(
    platform: &fbuild_packages::library::Esp32Platform,
    mcu: &str,
) -> (Option<String>, Option<String>) {
    let mcu_suffix = mcu.strip_prefix("esp32").unwrap_or("");
    let libs_url = platform
        .get_package_url("framework-arduinoespressif32-libs")
        .ok();
    let skeleton_url = if mcu_suffix.is_empty() {
        None
    } else {
        platform
            .get_package_url(&format!("framework-arduino-{}-skeleton-lib", mcu_suffix))
            .ok()
    };
    (libs_url, skeleton_url)
}

pub(super) async fn ensure_sdk_libs(
    framework: &fbuild_packages::library::Esp32Framework,
    mcu: &str,
    libs_url: Option<&str>,
    skeleton_url: Option<&str>,
) -> Result<()> {
    if let Some(url) = libs_url {
        framework.ensure_libs(url, mcu).await?;
    }
    if let Some(url) = skeleton_url {
        framework.ensure_mcu_libs(url, mcu).await?;
    }
    Ok(())
}

/// Name an unsupported non-archive URL instead of dropping it silently
/// (FastLED/fbuild#1407). Registry aliases are resolved above; other URLs
/// still fall back to the pioarduino stable platform.
fn warn_unhonored_platform_pin(env_config: Option<&HashMap<String, String>>) {
    let Some(value) = env_config.and_then(|env| env.get("platform")) else {
        return;
    };
    let value = value.trim();
    if value.contains("://") {
        tracing::warn!(
            "platform pin `{value}` is not a downloadable archive URL; building with the \
             pioarduino stable platform instead. Pin a release with `platform = \
             https://github.com/pioarduino/platform-espressif32/releases/download/<tag>/platform-espressif32.zip`"
        );
    }
}

/// Provision the managed `tool-esptoolpy` package (the tasmota PyInstaller
/// standalone binary) from `platform.json` and return the path to the
/// `esptool` executable.
///
/// Best-effort: a provisioning failure (missing `platform.json` entry,
/// unsupported host, network error) is reported at error level — naming the
/// parsed version and the URL that was tried — and yields `Ok(None)`, so the
/// linker's existing "esptool on PATH" fallback still applies. See
/// FastLED/fbuild#954.
///
/// The one *fatal* case is a bad `FBUILD_ESPTOOL_PATH`: an explicit override
/// that pointed nowhere used to be indistinguishable from no override at all.
/// It now fails the build immediately (FastLED/fbuild#1220).
pub(super) async fn resolve_esptool(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
) -> Result<Option<NormalizedPath>> {
    // Check the override before touching platform.json: when provisioning is
    // the thing that's broken, the metadata lookup on the way to it should not
    // be able to fail the resolution first.
    if let Some(path) = fbuild_packages::library::esptool_path_override()? {
        return Ok(Some(path));
    }

    let requirement = match platform.get_package_requirement("tool-esptoolpy") {
        Ok(requirement) => requirement,
        Err(e) => {
            tracing::error!(
                "esptool provisioning failed: could not resolve tool-esptoolpy \
                 from platform.json: {e}. Falling back to an `esptool` on PATH; \
                 set {} to override.",
                fbuild_packages::library::ESPTOOL_PATH_ENV_VAR
            );
            return Ok(None);
        }
    };
    let esptool = esptool_package(&requirement, project_dir, true)
        .await?
        .ok_or_else(|| {
            fbuild_core::FbuildError::PackageError("ESP32 esptool unavailable".into())
        })?;
    let url = esptool.download_url()?;
    match esptool.ensure_installed().await {
        Ok(path) => {
            tracing::info!("provisioned esptool at {}", path.display());
            Ok(Some(path))
        }
        Err(e) => {
            // Name the parsed version and the exact URL. In #1217 the real
            // fault was a 404 on a `vunknown` URL — invisible at default
            // verbosity, and three minutes later the build died claiming
            // esptool simply wasn't installed.
            let attempted = esptool
                .download_url()
                .unwrap_or_else(|_| "<no prebuilt binary for this host>".to_string());
            Err(fbuild_core::FbuildError::PackageError(format!(
                "pinned esptool provisioning failed: {e}; metadata URL: {url}; \
                 parsed version: {}; download URL: {attempted}; set {} to override",
                esptool.version(),
                fbuild_packages::library::ESPTOOL_PATH_ENV_VAR
            )))
        }
    }
}

pub(super) async fn esptool_package(
    requirement: &fbuild_core::platformio_package::PackageRequirement,
    project_dir: &Path,
    fetch: bool,
) -> Result<Option<fbuild_packages::library::Esptool>> {
    if let Some(registry) = requirement.spec.registry() {
        let host =
            fbuild_core::platformio_package::host_system(fbuild_core::platform::host::current())
                .ok_or_else(|| {
                    fbuild_core::FbuildError::PackageError("unsupported PlatformIO host".into())
                })?;
        let cache_root = fbuild_packages::Cache::new(project_dir).toolchains_dir();
        let Some(payload) = fbuild_packages::platformio_registry::RegistryClient::default()
            .resolve_cached(
                registry,
                RegistryPackageKind::Tool,
                host,
                &cache_root,
                fetch,
            )
            .await
            .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?
        else {
            return Ok(None);
        };
        return Ok(Some(
            fbuild_packages::library::Esptool::from_registry_payload(project_dir, &payload),
        ));
    }
    let url = match &requirement.spec.source {
        PackageSource::Archive { url, .. } => url,
        _ => {
            return Err(fbuild_core::FbuildError::PackageError(
                "unsupported esptool package source".into(),
            ));
        }
    };
    Ok(Some(fbuild_packages::library::Esptool::from_metadata_url(
        project_dir,
        url,
    )))
}

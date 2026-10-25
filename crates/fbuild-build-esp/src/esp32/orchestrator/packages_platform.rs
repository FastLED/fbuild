//! ESP32 platform, framework, SDK and esptool package selection.

use super::*;

fn explicit_framework_spec(
    env_config: Option<&HashMap<String, String>>,
) -> Result<Option<fbuild_core::platformio_package::PackageSpec>> {
    let Some(raw) = env_config.and_then(|env| env.get("platform_packages")) else {
        return Ok(None);
    };
    fbuild_config::parse_platform_packages_spec(raw, "framework-arduinoespressif32")
        .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))
}

fn framework_url_override(
    env_config: Option<&HashMap<String, String>>,
) -> Result<Option<fbuild_config::PackageOverride>> {
    if explicit_framework_spec(env_config)?
        .as_ref()
        .is_some_and(|spec| spec.registry().is_some())
    {
        return Ok(None);
    }
    Ok(env_config.and_then(|env| {
        crate::package_override::resolve_override(env, "framework-arduinoespressif32")
    }))
}

/// An explicit registry framework pin on an unversioned PlatformIO platform
/// must use that platform's registry manifest, not pioarduino stable's SDK.
fn platform_registry_spec(
    env_config: Option<&HashMap<String, String>>,
) -> Result<Option<RegistrySpec>> {
    let Some(env) = env_config else {
        return Ok(None);
    };
    let Some(platform) = env.get("platform").map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    let framework_registry_pin =
        explicit_framework_spec(Some(env))?.is_some_and(|spec| spec.registry().is_some());
    if !platform.contains('@') && !framework_registry_pin {
        return Ok(None);
    }
    if platform.starts_with("http://") || platform.starts_with("https://") {
        return Ok(None);
    }
    let spec = parse_package_spec(platform)
        .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?;
    let PackageSource::Registry(registry) = spec.source else {
        return Err(fbuild_core::FbuildError::PackageError(format!(
            "unsupported ESP32 platform source `{platform}`"
        )));
    };
    if registry.name != "espressif32" {
        return Err(fbuild_core::FbuildError::PackageError(format!(
            "unsupported ESP32 platform alias `{platform}`"
        )));
    }
    Ok(Some(registry))
}

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
    if let Some(registry) = platform_registry_spec(env_config)? {
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
    if let Some(source) = platform_repository_source(env_config)? {
        // A `.git#<ref>` pin is resolved to an immutable commit and installed
        // under that commit's archive URL, so two refs can never share a
        // cached payload, and a ref is never replaced by stable
        // (FastLED/fbuild#1662). The commit lock is reused by later builds.
        if !fetch {
            return Ok(None);
        }
        let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
        let resolved = fbuild_packages::platformio_repository::resolve_cached_source(
            &source,
            &cache_root,
            false,
        )
        .await
        .map_err(|error| {
            fbuild_core::FbuildError::PackageError(format!(
                "cannot resolve ESP32 platform `{}`: {error}",
                requested_platform(env_config)
            ))
        })?;
        tracing::info!(
            "resolved requested ESP32 platform {} to {}",
            requested_platform(env_config),
            resolved.archive.url
        );
        return Ok(Some(
            fbuild_packages::library::Esp32Platform::with_override(project_dir, resolved.archive),
        ));
    }
    reject_unhonored_platform_pin(env_config)?;
    Ok(Some(fbuild_packages::library::Esp32Platform::new(
        project_dir,
    )))
}

fn requested_platform(env_config: Option<&HashMap<String, String>>) -> &str {
    env_config
        .and_then(|env| env.get("platform"))
        .map_or("", |value| value.trim())
}

/// A `platform = <repository URL>[#ref]` pin. Archive URLs are handled by
/// `resolve_platform_override`; registry names by [`platform_registry_spec`].
fn platform_repository_source(
    env_config: Option<&HashMap<String, String>>,
) -> Result<Option<PackageSource>> {
    let platform = requested_platform(env_config);
    if !platform.contains("://") && !platform.starts_with("git@") {
        return Ok(None);
    }
    match parse_package_spec(platform) {
        Ok(spec) if matches!(spec.source, PackageSource::Repository { .. }) => {
            Ok(Some(spec.source))
        }
        _ => Ok(None),
    }
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
    if let Some(override_package) = framework_url_override(env_config)? {
        return Ok(Some(
            fbuild_packages::library::Esp32Framework::with_override(project_dir, override_package),
        ));
    }
    let explicit = explicit_framework_spec(env_config)?;
    let requirement = platform.get_package_requirement_with_overrides(
        "framework-arduinoespressif32",
        &explicit.into_iter().collect::<Vec<_>>(),
    )?;
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
/// A URL pin that is neither a release archive nor a repository cannot be
/// honored. Building pioarduino stable instead would report a green build of
/// an SDK the project did not ask for (FastLED/fbuild#1662).
fn reject_unhonored_platform_pin(env_config: Option<&HashMap<String, String>>) -> Result<()> {
    let value = requested_platform(env_config);
    if value.contains("://") {
        return Err(fbuild_core::FbuildError::PackageError(format!(
            "ESP32 platform pin `{value}` is neither a release archive nor a repository URL. \
             Pin a release with `platform = \
             https://github.com/pioarduino/platform-espressif32/releases/download/<tag>/platform-espressif32.zip` \
             or a repository with `platform = https://github.com/<owner>/<repo>.git#<ref>`"
        )));
    }
    Ok(())
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

#[cfg(test)]
mod registry_framework_override_tests {
    use super::*;

    fn platform_env(platform: &str) -> HashMap<String, String> {
        HashMap::from([("platform".into(), platform.into())])
    }

    #[test]
    fn repository_ref_pin_is_a_repository_source() {
        let env = platform_env("https://github.com/pioarduino/platform-espressif32.git#prep_IDF6");
        let Some(PackageSource::Repository { url, reference }) =
            platform_repository_source(Some(&env)).unwrap()
        else {
            panic!("expected a repository source");
        };
        assert_eq!(
            url,
            "https://github.com/pioarduino/platform-espressif32.git"
        );
        assert_eq!(reference.as_deref(), Some("prep_IDF6"));
    }

    #[test]
    fn archive_and_registry_pins_are_not_repository_sources() {
        for platform in [
            "https://github.com/pioarduino/platform-espressif32/releases/download/55.03.311/platform-espressif32.zip",
            "espressif32@6.9.0",
            "espressif32",
        ] {
            assert!(
                platform_repository_source(Some(&platform_env(platform)))
                    .unwrap()
                    .is_none(),
                "{platform}"
            );
        }
    }

    #[test]
    fn unresolvable_url_pin_is_an_error_not_a_stable_fallback() {
        let env = platform_env("https://example.test/not-a-platform");
        assert!(reject_unhonored_platform_pin(Some(&env)).is_err());
        assert!(reject_unhonored_platform_pin(Some(&platform_env("espressif32"))).is_ok());
        assert!(reject_unhonored_platform_pin(None).is_ok());
    }

    #[tokio::test]
    async fn cached_commit_lock_selects_that_commits_archive_without_network() {
        let tmp = tempfile::TempDir::new().unwrap();
        let env = platform_env("https://github.com/pioarduino/platform-espressif32.git#prep_IDF6");
        let source = platform_repository_source(Some(&env)).unwrap().unwrap();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let request = serde_json::to_vec(&source).unwrap();
        let key = format!("{:x}", Sha256::digest(request));
        let cache_root = fbuild_packages::Cache::new(tmp.path()).platforms_dir();
        let locks = cache_root.join("platformio-source-locks");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(
            locks.join(format!("{key}.json")),
            serde_json::to_vec(&fbuild_core::platformio_package::PackageLock::Repository {
                url: "https://github.com/pioarduino/platform-espressif32.git".into(),
                commit: commit.into(),
            })
            .unwrap(),
        )
        .unwrap();
        let platform = pioarduino_platform(tmp.path(), Some(&env), true)
            .await
            .unwrap()
            .unwrap();
        let url = fbuild_packages::Package::get_info(&platform).url;
        assert!(url.contains(commit), "{url}");
    }

    #[test]
    fn framework_registry_pin_selects_registry_platform_for_unversioned_alias() {
        let env = HashMap::from([
            ("platform".into(), "espressif32".into()),
            (
                "platform_packages".into(),
                "framework-arduinoespressif32@3.20017.241212+sha.dcc1105b".into(),
            ),
        ]);
        let registry = platform_registry_spec(Some(&env)).unwrap().unwrap();
        assert_eq!(registry.name, "espressif32");
        assert_eq!(registry.requirement, None);

        let url_pin = HashMap::from([
            (
                "platform".into(),
                "https://example.test/pioarduino/platform.zip".into(),
            ),
            (
                "platform_packages".into(),
                "framework-arduinoespressif32@3.20017.241212+sha.dcc1105b".into(),
            ),
        ]);
        assert!(platform_registry_spec(Some(&url_pin)).unwrap().is_none());
    }

    #[test]
    fn first_framework_entry_controls_registry_vs_url_precedence() {
        let registry_first = HashMap::from([
            ("platform".into(), "espressif32".into()),
            (
                "platform_packages".into(),
                "framework-arduinoespressif32@3.20017.241212+sha.dcc1105b\nframework-arduinoespressif32@https://example.test/framework.tar.gz".into(),
            ),
        ]);
        assert!(
            platform_registry_spec(Some(&registry_first))
                .unwrap()
                .is_some()
        );
        assert!(
            framework_url_override(Some(&registry_first))
                .unwrap()
                .is_none()
        );

        let url_first = HashMap::from([
            ("platform".into(), "espressif32".into()),
            (
                "platform_packages".into(),
                "framework-arduinoespressif32@https://example.test/framework.tar.gz\nframework-arduinoespressif32@3.20017.241212+sha.dcc1105b".into(),
            ),
        ]);
        assert!(platform_registry_spec(Some(&url_first)).unwrap().is_none());
        assert!(framework_url_override(Some(&url_first)).unwrap().is_some());
    }
}

//! Shared `platform_packages` override resolver for every framework orchestrator.
//!
//! PlatformIO honors `platform_packages = framework-x@<URL>#<sha>` on the env
//! section; fbuild's framework packages used to ignore it (audit: #664; first
//! report: #663). The per-orchestrator wiring is now a uniform three-line delta:
//!
//! ```ignore
//! let core = match package_override::resolve_override(env_config, "framework-arduino-lpc8xx") {
//!     Some(ovr) => ArduinoCoreLpc8xx::with_override(&params.project_dir, ovr),
//!     None      => ArduinoCoreLpc8xx::new(&params.project_dir),
//! };
//! ```
//!
//! Centralizing the lookup here means every framework orchestrator gets the
//! same parsing behavior — there is no per-platform place to forget multi-line
//! handling, owner/repo expansion, or empty-value tolerance.

use std::collections::HashMap;
use std::path::Path;

use fbuild_config::PackageOverride;
use fbuild_core::platformio_package::{
    PackageKind, PackageRequirement, PackageSource, PackageSpec, parse_package_spec,
    require_platform_package, resolve_platform_requirements,
};

/// Resolve a pinned PlatformIO platform and its selected package requirements
/// before a family adapter constructs its framework/toolchain packages. The
/// registry lookup and manifest parsing are intentionally independent of the
/// native platform enum; adapters only provide the package names they use.
pub async fn resolve_registry_overrides(
    project_dir: &Path,
    env_config: &HashMap<String, String>,
    platform_name: &str,
    package_names: &[&str],
    platform_default_requirements: &[(&str, &str)],
) -> fbuild_core::Result<HashMap<String, PackageOverride>> {
    let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
    let client = fbuild_packages::platformio_registry::RegistryClient::default();
    resolve_registry_overrides_with_client(
        project_dir,
        env_config,
        platform_name,
        package_names,
        platform_default_requirements,
        &client,
        &cache_root,
    )
    .await
}

async fn resolve_registry_overrides_with_client(
    project_dir: &Path,
    env_config: &HashMap<String, String>,
    platform_name: &str,
    package_names: &[&str],
    platform_default_requirements: &[(&str, &str)],
    client: &fbuild_packages::platformio_registry::RegistryClient,
    cache_root: &Path,
) -> fbuild_core::Result<HashMap<String, PackageOverride>> {
    let raw_platform = env_config
        .get("platform")
        .map(String::as_str)
        .unwrap_or(platform_name)
        .trim();
    let platform_spec = parse_package_spec(raw_platform).map_err(package_error)?;
    // Parse every entry through the generic core grammar before the native
    // adapter filters package names. Otherwise an explicit registry pin for a
    // package the adapter forgot to list is silently ignored and the build
    // proceeds with its default stack.
    let mut explicit = Vec::<PackageSpec>::new();
    if let Some(raw) = env_config.get("platform_packages") {
        for line in raw.lines() {
            let entry = line.trim().trim_end_matches([',', ';']).trim();
            if entry.is_empty() {
                continue;
            }
            let spec = parse_package_spec(entry).map_err(package_error)?;
            let name = spec.package_name().ok_or_else(|| {
                fbuild_core::FbuildError::PackageError(format!(
                    "PlatformIO platform_packages entry `{entry}` has no package name"
                ))
            })?;
            if !package_names.contains(&name) {
                if spec.registry().is_some() {
                    return Err(fbuild_core::FbuildError::PackageError(format!(
                        "PlatformIO package `{name}` is not supported by the `{platform_name}` adapter"
                    )));
                }
                continue;
            }
            if !explicit
                .iter()
                .any(|prior| prior.package_name() == Some(name))
            {
                explicit.push(spec);
            }
        }
    }
    let has_registry_pin = platform_spec
        .registry()
        .and_then(|registry| registry.requirement.as_ref())
        .is_some()
        || explicit.iter().any(|spec| spec.registry().is_some());
    if !has_registry_pin {
        return Ok(HashMap::new());
    }
    let host = fbuild_core::platformio_package::host_system(fbuild_core::platform::host::current())
        .ok_or_else(|| {
            fbuild_core::FbuildError::PackageError("unsupported PlatformIO host".into())
        })?;
    let registry_platform = platform_spec
        .registry()
        .is_some_and(|spec| spec.requirement.is_some());
    let requirements = if let PackageSource::Registry(platform_registry) = platform_spec.source {
        if platform_registry.name != platform_name {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "expected PlatformIO platform `{platform_name}`, got `{}`",
                platform_registry.name
            )));
        }
        if platform_registry.requirement.is_none() {
            // An explicit package pin on an unpinned platform should not
            // silently select a newer registry platform/manifest than the
            // native adapter's existing default stack.
            explicit_registry_requirements(&explicit)
        } else {
            // PlatformIO platform.py can specialize package requirements by board
            // and framework. Keep consumer overrides first.
            for (name, requirement) in platform_default_requirements {
                explicit.push(
                    parse_package_spec(&format!("{name}@{requirement}")).map_err(package_error)?,
                );
            }
            let platform = client
                .resolve_cached(
                    &platform_registry,
                    PackageKind::Platform,
                    host,
                    cache_root,
                    true,
                )
                .await
                .map_err(package_error)?
                .ok_or_else(|| {
                    fbuild_core::FbuildError::PackageError("PlatformIO platform unavailable".into())
                })?;
            tracing::info!(
                "resolved requested PlatformIO platform {}@{}: {} (sha256 {})",
                platform.name,
                platform.version,
                platform.url,
                platform.sha256
            );
            let platform_base = fbuild_packages::PackageBase::new(
                &platform.name,
                &platform.version,
                &platform.url,
                &platform.cache_identity(),
                Some(&platform.sha256),
                fbuild_packages::CacheSubdir::Platforms,
                project_dir,
            );
            let installed = platform_base
                .staged_install(|dir| {
                    find_platform_manifest(dir).ok_or_else(|| {
                        fbuild_core::FbuildError::PackageError(format!(
                            "{} has no platform.json",
                            dir.display()
                        ))
                    })?;
                    Ok(())
                })
                .await?;
            let manifest_path = find_platform_manifest(&installed).ok_or_else(|| {
                fbuild_core::FbuildError::PackageError(format!(
                    "{} has no platform.json",
                    installed.display()
                ))
            })?;
            let manifest = std::fs::read_to_string(manifest_path)
                .map_err(|error| fbuild_core::FbuildError::PackageError(error.to_string()))?;
            resolve_platform_requirements(&manifest, &explicit).map_err(package_error)?
        }
    } else {
        // A platform URL, repository ref, or local directory remains the
        // platform source. Its explicitly registry-pinned packages still need
        // payload resolution; no registry platform manifest is implied.
        explicit_registry_requirements(&explicit)
    };
    let mut overrides = HashMap::new();
    for name in package_names {
        if explicit
            .iter()
            .any(|spec| spec.package_name() == Some(name) && spec.registry().is_none())
        {
            continue;
        }
        let requirement = if registry_platform {
            Some(require_platform_package(&requirements, name).map_err(package_error)?)
        } else {
            requirements
                .iter()
                .find(|requirement| requirement.name == *name)
        };
        let Some(requirement) = requirement else {
            continue;
        };
        let Some(registry) = requirement.spec.registry() else {
            continue;
        };
        let payload = client
            .resolve_cached(registry, requirement.kind, host, cache_root, true)
            .await
            .map_err(package_error)?
            .ok_or_else(|| {
                fbuild_core::FbuildError::PackageError(format!(
                    "PlatformIO package `{name}` unavailable"
                ))
            })?;
        tracing::info!(
            "resolved PlatformIO package {}@{}: {} (sha256 {})",
            payload.name,
            payload.version,
            payload.url,
            payload.sha256
        );
        overrides.insert(
            (*name).to_string(),
            PackageOverride {
                url: payload.url,
                version: payload.version,
                checksum: Some(payload.sha256),
            },
        );
    }
    Ok(overrides)
}

fn explicit_registry_requirements(explicit: &[PackageSpec]) -> Vec<PackageRequirement> {
    explicit
        .iter()
        .filter_map(|spec| {
            spec.registry().map(|_| {
                let name = spec.package_name().unwrap_or_default();
                PackageRequirement {
                    name: name.to_string(),
                    kind: package_kind_from_name(name),
                    optional: false,
                    spec: spec.clone(),
                }
            })
        })
        .collect()
}

fn package_kind_from_name(name: &str) -> PackageKind {
    if name.starts_with("framework-") {
        PackageKind::Framework
    } else if name.starts_with("platform-") {
        PackageKind::Platform
    } else {
        PackageKind::Tool
    }
}

fn find_platform_manifest(root: &Path) -> Option<std::path::PathBuf> {
    walkdir::WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .find(|entry| entry.file_type().is_file() && entry.file_name() == "platform.json")
        .map(|entry| entry.path().to_path_buf())
}

fn package_error(error: impl std::fmt::Display) -> fbuild_core::FbuildError {
    fbuild_core::FbuildError::PackageError(error.to_string())
}

/// Look up a `platform_packages` override for `package_name` in the resolved
/// env config (`PlatformIOConfig::get_env_config(env)`).
///
/// Returns `None` when the env has no `platform_packages` key, or when no entry
/// in that value matches `package_name`. Multi-line values are scanned in order
/// and the first match wins (PlatformIO semantics).
pub fn resolve_override(
    env_config: &HashMap<String, String>,
    package_name: &str,
) -> Option<PackageOverride> {
    let raw = env_config.get("platform_packages")?;
    fbuild_config::parse_platform_packages_value(raw, package_name)
}

/// Look up the pin for a platform package such as `platform-espressif32`.
///
/// A `platform_packages` entry wins, as in PlatformIO. Otherwise a
/// `platform = <archive URL>` pin applies: ignoring it silently built against a
/// different framework release than the one the ini named (FastLED/fbuild#1432).
pub fn resolve_platform_override(
    env_config: &HashMap<String, String>,
    package_name: &str,
) -> Option<PackageOverride> {
    resolve_override(env_config, package_name)
        .or_else(|| fbuild_config::parse_platform_archive_url(env_config.get("platform")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn returns_override_when_env_has_matching_entry() {
        let env = env(&[(
            "platform_packages",
            "framework-arduino-lpc8xx@https://github.com/zackees/ArduinoCore-LPC8xx/archive/aaaabbbbccccddddeeeeffff0000111122223333.tar.gz#aaaabbbbccccddddeeeeffff0000111122223333",
        )]);
        let ovr = resolve_override(&env, "framework-arduino-lpc8xx").expect("override resolved");
        assert!(ovr.url.contains("ArduinoCore-LPC8xx"));
        assert_eq!(ovr.version, "0.0.0+gaaaabbb");
    }

    #[test]
    fn returns_none_when_platform_packages_key_absent() {
        let env = env(&[("build_flags", "-DFOO=1")]);
        assert!(resolve_override(&env, "framework-arduino-lpc8xx").is_none());
    }

    #[test]
    fn returns_none_when_no_entry_matches_package_name() {
        let env = env(&[(
            "platform_packages",
            "framework-some-other-thing@https://example.com/archive/abc.tar.gz#abc",
        )]);
        assert!(resolve_override(&env, "framework-arduino-lpc8xx").is_none());
    }

    #[test]
    fn multi_line_value_picks_first_matching_entry() {
        // PlatformIO INI parsing joins continuation lines with `\n`; the
        // helper must scan all of them and return the first match.
        let env = env(&[(
            "platform_packages",
            "framework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\nframework-arduino-lpc8xx@zackees/ArduinoCore-LPC8xx#cafef00dcafef00dcafef00dcafef00dcafef00d",
        )]);
        let ovr = resolve_override(&env, "framework-arduino-lpc8xx").unwrap();
        assert_eq!(ovr.version, "0.0.0+gdeadbee");
    }

    const PLATFORM_54: &str = "https://github.com/pioarduino/platform-espressif32/releases/download/54.03.20/platform-espressif32.zip";
    const PLATFORM_55: &str = "https://github.com/pioarduino/platform-espressif32/releases/download/55.03.35/platform-espressif32.zip";

    #[test]
    fn platform_archive_url_pins_the_platform_package() {
        let env = env(&[("platform", PLATFORM_54)]);
        let ovr = resolve_platform_override(&env, "platform-espressif32").expect("pin honored");
        assert_eq!(ovr.url, PLATFORM_54);
        assert_eq!(ovr.version, "54.03.20");
    }

    #[test]
    fn platform_packages_entry_wins_over_platform_url() {
        let packages = format!("platform-espressif32@{PLATFORM_55}");
        let env = env(&[("platform", PLATFORM_54), ("platform_packages", &packages)]);
        let ovr = resolve_platform_override(&env, "platform-espressif32").unwrap();
        assert_eq!(ovr.url, PLATFORM_55);
    }

    #[test]
    fn unpinned_platform_name_resolves_no_override() {
        let env = env(&[("platform", "espressif32")]);
        assert!(resolve_platform_override(&env, "platform-espressif32").is_none());
    }

    #[test]
    fn version_pin_only_returns_none() {
        // `name @ 1.2.3` is a registry version pin, not a URL override.
        let env = env(&[("platform_packages", "framework-arduino-lpc8xx @ 1.2.3")]);
        assert!(resolve_override(&env, "framework-arduino-lpc8xx").is_none());
    }

    #[test]
    fn repository_platform_keeps_explicit_registry_package_resolution() {
        let platform =
            parse_package_spec("https://github.com/maxgerhardt/platform-raspberrypi.git").unwrap();
        assert!(matches!(platform.source, PackageSource::Repository { .. }));
        let framework = parse_package_spec("platformio/framework-arduino-mbed@4.6.0").unwrap();
        let toolchain =
            parse_package_spec("platformio/toolchain-gccarmnoneeabi@1.90201.0").unwrap();
        let url_override =
            parse_package_spec("framework-arduinopico@https://example.test/arduino-pico.tar.gz")
                .unwrap();
        let requirements = explicit_registry_requirements(&[framework, toolchain, url_override]);
        assert_eq!(requirements.len(), 2);
        assert_eq!(requirements[0].name, "framework-arduino-mbed");
        assert_eq!(requirements[0].kind, PackageKind::Framework);
        assert_eq!(requirements[1].name, "toolchain-gccarmnoneeabi");
        assert_eq!(requirements[1].kind, PackageKind::Tool);
    }

    #[tokio::test]
    async fn repository_platform_resolves_pinned_package_without_replacing_platform_source() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let bytes = socket.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..bytes]).unwrap();
            assert!(
                request.starts_with("GET /v3/packages/platformio/tool/framework-arduino-mbed ")
            );
            let body = r#"{"name":"framework-arduino-mbed","owner":{"username":"platformio"},"versions":[{"name":"4.6.0","files":[{"system":"*","download_url":"https://example.test/framework-arduino-mbed-4.6.0.tar.gz","checksum":{"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}]}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let client = fbuild_packages::platformio_registry::RegistryClient::new(
            format!("http://{address}/v3"),
            reqwest::Client::new(),
        );
        let temp = tempfile::tempdir().unwrap();
        let env = env(&[
            (
                "platform",
                "https://github.com/maxgerhardt/platform-raspberrypi.git",
            ),
            (
                "platform_packages",
                "platformio/framework-arduino-mbed@4.6.0",
            ),
        ]);
        let overrides = resolve_registry_overrides_with_client(
            temp.path(),
            &env,
            "raspberrypi",
            &["framework-arduino-mbed"],
            &[],
            &client,
            &temp.path().join("cache"),
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(overrides["framework-arduino-mbed"].version, "4.6.0");
        assert_eq!(
            overrides["framework-arduino-mbed"].url,
            "https://example.test/framework-arduino-mbed-4.6.0.tar.gz"
        );
        assert_eq!(
            overrides["framework-arduino-mbed"].checksum.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[tokio::test]
    async fn unpinned_platform_keeps_explicit_registry_package_without_fetching_platform() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let bytes = socket.read(&mut request).await.unwrap();
            let request = std::str::from_utf8(&request[..bytes]).unwrap();
            assert!(
                request.starts_with("GET /v3/packages/platformio/tool/toolchain-gccarmnoneeabi ")
            );
            let body = r#"{"name":"toolchain-gccarmnoneeabi","owner":{"username":"platformio"},"versions":[{"name":"1.90201.0","files":[{"system":"*","download_url":"https://example.test/arm-gcc.tar.gz","checksum":{"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}]}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let client = fbuild_packages::platformio_registry::RegistryClient::new(
            format!("http://{address}/v3"),
            reqwest::Client::new(),
        );
        let temp = tempfile::tempdir().unwrap();
        let env = env(&[
            ("platform", "nxplpc"),
            (
                "platform_packages",
                "platformio/toolchain-gccarmnoneeabi@1.90201.0",
            ),
        ]);
        let overrides = resolve_registry_overrides_with_client(
            temp.path(),
            &env,
            "nxplpc",
            &["framework-arduino-lpc8xx", "toolchain-gccarmnoneeabi"],
            &[],
            &client,
            &temp.path().join("cache"),
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(overrides["toolchain-gccarmnoneeabi"].version, "1.90201.0");
    }

    #[tokio::test]
    async fn explicit_registry_pin_outside_adapter_packages_fails_instead_of_using_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let client = fbuild_packages::platformio_registry::RegistryClient::new(
            "http://127.0.0.1:1/v3",
            reqwest::Client::new(),
        );
        let env = env(&[
            ("platform", "nxplpc"),
            (
                "platform_packages",
                "platformio/tool/framework-arduino-unknown@1.2.3",
            ),
        ]);
        let result = resolve_registry_overrides_with_client(
            temp.path(),
            &env,
            "nxplpc",
            &["toolchain-gccarmnoneeabi"],
            &[],
            &client,
            &temp.path().join("cache"),
        )
        .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("framework-arduino-unknown")
        );
    }
}

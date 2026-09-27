//! Select the packages described by a CH32V PlatformIO source checkout.

use std::collections::HashMap;
use std::path::Path;

use fbuild_config::PackageOverride;
use fbuild_core::path::NormalizedPath;
use fbuild_core::platformio_package::{
    PackageKind, PackageRequirement, PackageSource, PackageSpec, parse_package_spec,
    require_platform_package, resolve_platform_requirements,
};
use fbuild_packages::{CacheSubdir, PackageBase};

#[derive(Debug)]
pub(crate) struct SelectedRequirements {
    pub framework: PackageRequirement,
    pub toolchain: PackageRequirement,
}

#[derive(Debug, Clone)]
pub(crate) struct SelectedBoardBuild {
    pub core: String,
    pub variant: String,
    pub variant_h: Option<String>,
    pub march: String,
    pub mabi: String,
}

/// Resolve an explicitly selected CH32V platform source before constructing
/// any native packages. Unpinned `platform = ch32v` keeps the legacy native
/// defaults; a nonexistent registry pin never reaches that fallback.
pub(crate) async fn resolve_source_packages(
    project_dir: &Path,
    env: &HashMap<String, String>,
    board_core: &str,
    refresh: bool,
) -> fbuild_core::Result<
    Option<(
        fbuild_packages::toolchain::RiscvToolchain,
        fbuild_packages::library::Ch32vCores,
        SelectedBoardBuild,
        String,
    )>,
> {
    let raw_platform = env.get("platform").map(String::as_str).unwrap_or("ch32v");
    let platform = parse_package_spec(raw_platform).map_err(package_error)?;
    let host = fbuild_core::platformio_package::host_system(fbuild_core::platform::host::current())
        .ok_or_else(|| package_error("unsupported PlatformIO host"))?;
    let (archive, identity) = match &platform.source {
        PackageSource::Registry(registry) if is_legacy_default(registry) => {
            return Ok(None);
        }
        PackageSource::Registry(registry) => {
            let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
            let payload = fbuild_packages::platformio_registry::RegistryClient::default()
                .resolve_cached(registry, PackageKind::Platform, host, &cache_root, true)
                .await
                .map_err(package_error)?
                .ok_or_else(|| package_error("CH32V platform registry pin is unavailable"))?;
            (
                PackageOverride {
                    url: payload.url.clone(),
                    version: payload.version.clone(),
                    checksum: Some(payload.sha256.clone()),
                },
                payload.cache_identity(),
            )
        }
        PackageSource::Repository { .. } | PackageSource::Archive { .. } => {
            let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
            let resolved = fbuild_packages::platformio_repository::resolve_cached_source(
                &platform.source,
                &cache_root,
                refresh,
            )
            .await
            .map_err(package_error)?;
            let identity = resolved.lock.cache_identity();
            (resolved.archive, identity)
        }
        PackageSource::LocalPath { .. } => {
            return Err(package_error(
                "local CH32V platform directories are not supported by the native adapter",
            ));
        }
    };
    let platform_base = PackageBase::new(
        "ch32v-platform",
        &archive.version,
        &archive.url,
        &identity,
        archive.checksum.as_deref(),
        CacheSubdir::Platforms,
        project_dir,
    );
    let installed = platform_base
        .staged_install(|root| {
            find_platform_manifest(root)
                .ok_or_else(|| package_error("selected CH32V platform has no platform.json"))?;
            Ok(())
        })
        .await?;
    let manifest_path = find_platform_manifest(&installed)
        .ok_or_else(|| package_error("selected CH32V platform has no platform.json"))?;
    let manifest = std::fs::read_to_string(&manifest_path).map_err(package_error)?;
    let declared_version = serde_json::from_str::<serde_json::Value>(&manifest)
        .map_err(package_error)?
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| package_error("selected CH32V platform has no version"))?
        .to_string();
    let builder = std::fs::read_to_string(manifest_path.with_file_name("platform.py"))
        .map_err(package_error)?;
    let board_name = env
        .get("board")
        .ok_or_else(|| package_error("CH32V platform source requires a board name"))?;
    if !board_name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(package_error(format!(
            "invalid CH32V board name `{board_name}`"
        )));
    }
    let board_path = manifest_path
        .parent()
        .ok_or_else(|| package_error("CH32V platform manifest has no parent"))?
        .join("boards")
        .join(format!("{board_name}.json"));
    let board: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&board_path).map_err(package_error)?)
            .map_err(package_error)?;
    let board_build = board
        .get("build")
        .ok_or_else(|| package_error("selected CH32V board has no build metadata"))?;
    let selected_board = select_board_build(board_build, env, board_core)?;
    let selected_core = selected_board.core.as_str();
    let explicit = parse_explicit_packages(env)?;
    let selected = select_requirements(&manifest, &builder, selected_core, host, &explicit)?;
    if selected.framework.name != "framework-arduino-openwch-ch32" {
        return Err(package_error(format!(
            "CH32V core `{selected_core}` selects `{}`; the native adapter only supports OpenWCH",
            selected.framework.name
        )));
    }
    for entry in &explicit {
        let name = entry.package_name().unwrap_or_default();
        if name != selected.framework.name && name != selected.toolchain.name {
            return Err(package_error(format!(
                "CH32V platform package `{name}` is not consumed by the native adapter"
            )));
        }
    }
    let toolchain_override = package_override_for_spec(
        project_dir,
        &selected.toolchain.spec,
        PackageKind::Tool,
        host,
        refresh,
    )
    .await?;
    let framework_override = package_override_for_spec(
        project_dir,
        &selected.framework.spec,
        PackageKind::Framework,
        host,
        refresh,
    )
    .await?;
    tracing::info!(
        "resolved CH32V platform {} to {} (identity {}), toolchain {} at {}, framework {} at {}",
        raw_platform,
        archive.url,
        identity,
        selected.toolchain.name,
        toolchain_override.url,
        selected.framework.name,
        framework_override.url,
    );
    let platform_resolution = format!(
        "{}@{} (source_ref={}; url={}{})",
        platform.package_name().unwrap_or("ch32v"),
        declared_version,
        archive.version,
        archive.url,
        archive
            .checksum
            .as_ref()
            .map(|sha| format!("; sha256={sha}"))
            .unwrap_or_default()
    );
    Ok(Some((
        fbuild_packages::toolchain::RiscvToolchain::with_platform_override(
            project_dir,
            toolchain_override,
            "auto",
        ),
        fbuild_packages::library::Ch32vCores::with_platform_override(
            project_dir,
            framework_override,
        ),
        selected_board,
        platform_resolution,
    )))
}

fn select_board_build(
    board_build: &serde_json::Value,
    env: &HashMap<String, String>,
    fallback_core: &str,
) -> fbuild_core::Result<SelectedBoardBuild> {
    let core = env
        .get("board_build.core")
        .map(String::as_str)
        .or_else(|| board_build.get("core").and_then(serde_json::Value::as_str))
        .unwrap_or(fallback_core);
    let arduino_core = board_build.get("arduino").and_then(|value| value.get(core));
    let variant_override_key = format!("board_build.arduino.{core}.variant");
    let variant_h_override_key = format!("board_build.arduino.{core}.variant_h");
    let variant = env
        .get(&variant_override_key)
        .or_else(|| env.get("board_build.variant"))
        .map(String::as_str)
        .or_else(|| {
            arduino_core
                .and_then(|value| value.get("variant"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            board_build
                .get("variant")
                .and_then(serde_json::Value::as_str)
        })
        .ok_or_else(|| package_error("selected CH32V board has no variant"))?;
    let variant_h = env
        .get(&variant_h_override_key)
        .or_else(|| env.get("board_build.variant_h"))
        .map(String::as_str)
        .or_else(|| {
            arduino_core
                .and_then(|value| value.get("variant_h"))
                .and_then(serde_json::Value::as_str)
        });
    Ok(SelectedBoardBuild {
        core: core.into(),
        variant: variant.into(),
        variant_h: variant_h.map(str::to_string),
        march: board_build
            .get("march")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| package_error("selected CH32V board has no march"))?
            .into(),
        mabi: board_build
            .get("mabi")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| package_error("selected CH32V board has no mabi"))?
            .into(),
    })
}

fn is_legacy_default(registry: &fbuild_core::platformio_package::RegistrySpec) -> bool {
    registry.requirement.is_none()
        && registry.owner.is_none()
        && registry.registry_type.is_none()
        && registry.name == "ch32v"
}

fn find_platform_manifest(root: &Path) -> Option<NormalizedPath> {
    walkdir::WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| entry.file_type().is_file() && entry.file_name() == "platform.json")
        .map(|entry| NormalizedPath::from(entry.path()))
}

fn parse_explicit_packages(env: &HashMap<String, String>) -> fbuild_core::Result<Vec<PackageSpec>> {
    env.get("platform_packages")
        .into_iter()
        .flat_map(|value| value.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| parse_package_spec(line).map_err(package_error))
        .collect()
}

async fn package_override_for_spec(
    project_dir: &Path,
    spec: &PackageSpec,
    kind: PackageKind,
    host: &str,
    refresh: bool,
) -> fbuild_core::Result<PackageOverride> {
    match &spec.source {
        PackageSource::Registry(registry) => {
            let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
            let payload = fbuild_packages::platformio_registry::RegistryClient::default()
                .resolve_cached(registry, kind, host, &cache_root, true)
                .await
                .map_err(package_error)?
                .ok_or_else(|| package_error("CH32V platform package is unavailable"))?;
            Ok(PackageOverride {
                url: payload.url,
                version: payload.version,
                checksum: Some(payload.sha256),
            })
        }
        PackageSource::Repository { .. } | PackageSource::Archive { .. } => {
            let cache_root = fbuild_packages::Cache::new(project_dir).platforms_dir();
            Ok(
                fbuild_packages::platformio_repository::resolve_cached_source(
                    &spec.source,
                    &cache_root,
                    refresh,
                )
                .await
                .map_err(package_error)?
                .archive,
            )
        }
        PackageSource::LocalPath { .. } => Err(package_error(
            "local CH32V package directories are not supported by the native adapter",
        )),
    }
}

/// Apply the selected platform's own `platform.py` board/host choices to its
/// manifest, then give consumer `platform_packages` overrides final precedence.
/// This recognizes literal CH32V builder assignments; unexpected dynamic code
/// fails rather than silently retaining the manifest's Windows toolchain.
pub(crate) fn select_requirements(
    manifest: &str,
    builder: &str,
    board_core: &str,
    host: &str,
    explicit: &[PackageSpec],
) -> fbuild_core::Result<SelectedRequirements> {
    let framework_condition = format!("build_core == \"{board_core}\"");
    let framework_name = branch_assignment(
        builder,
        &framework_condition,
        "self.frameworks[\"arduino\"][\"package\"]",
    )
    .ok_or_else(|| {
        package_error(format!(
            "CH32V platform has no Arduino package for core `{board_core}`"
        ))
    })?;

    let mut selections = explicit.to_vec();
    if !explicit
        .iter()
        .any(|spec| spec.package_name() == Some("toolchain-riscv"))
    {
        let template = toolchain_template(
            builder,
            host,
            "self.packages[\"toolchain-riscv\"][\"version\"]",
        )
        .ok_or_else(|| {
            package_error(format!("CH32V platform has no toolchain for host `{host}`"))
        })?;
        let host_condition = format!("sys_type == \"{host}\"");
        let gcc_branch =
            branch_assignment(builder, "\"arduino\" in selected_frameworks", "gcc_branch")
                .or_else(|| branch_assignment(builder, &host_condition, "gcc_branch"))
                .or_else(|| root_assignment(builder, "gcc_branch"))
                .ok_or_else(|| package_error("CH32V platform has no GCC branch selection"))?;
        let source = template
            .strip_suffix("%s")
            .ok_or_else(|| package_error("CH32V toolchain builder expression is unsupported"))?;
        let version = format!("{source}{gcc_branch}");
        selections.push(
            parse_package_spec(&format!("toolchain-riscv@{version}")).map_err(package_error)?,
        );
    }
    let requirements =
        resolve_platform_requirements(manifest, &selections).map_err(package_error)?;
    Ok(SelectedRequirements {
        framework: require_platform_package(&requirements, &framework_name)
            .map_err(package_error)?
            .clone(),
        toolchain: require_platform_package(&requirements, "toolchain-riscv")
            .map_err(package_error)?
            .clone(),
    })
}

fn toolchain_template(builder: &str, host: &str, assignment: &str) -> Option<String> {
    // CH32V 1.1.0 uses IS_LINUX/IS_MAC/else. The manifest's Windows URL
    // cannot stand in for an unsupported host.
    match host {
        "linux_x86_64" => branch_assignment(builder, "IS_LINUX", assignment),
        "darwin_x86_64" | "darwin_arm64" => branch_assignment(builder, "IS_MAC", assignment),
        "windows_amd64" | "windows_x86" => {
            branch_assignment_after(builder, "elif IS_MAC:", "else:", assignment)
        }
        _ => None,
    }
}

fn branch_assignment_after(
    source: &str,
    preceding: &str,
    branch: &str,
    assignment: &str,
) -> Option<String> {
    let lines: Vec<_> = source.lines().collect();
    let prior = lines.iter().position(|line| line.trim() == preceding)?;
    let indent = indentation(lines[prior]);
    let branch_index = lines
        .iter()
        .enumerate()
        .skip(prior + 1)
        .find_map(|(index, line)| {
            (line.trim() == branch && indentation(line) == indent).then_some(index)
        })?;
    for line in lines.iter().skip(branch_index + 1) {
        if !line.trim().is_empty() && indentation(line) <= indent {
            break;
        }
        if let Some(value) = quoted_assignment(line.trim(), assignment) {
            return Some(value);
        }
    }
    None
}

fn branch_assignment(source: &str, condition: &str, assignment: &str) -> Option<String> {
    let lines: Vec<_> = source.lines().collect();
    for (branch_index, branch) in lines.iter().enumerate() {
        let trimmed = branch.trim();
        if !(trimmed.starts_with("if ") || trimmed.starts_with("elif "))
            || !trimmed.ends_with(':')
            || !trimmed.contains(condition)
        {
            continue;
        }
        let branch_indent = indentation(branch);
        for line in lines.iter().skip(branch_index + 1) {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if indentation(line) <= branch_indent {
                break;
            }
            if let Some(value) = quoted_assignment(trimmed, assignment) {
                return Some(value);
            }
        }
    }
    None
}

fn root_assignment(source: &str, assignment: &str) -> Option<String> {
    source
        .lines()
        .find_map(|line| quoted_assignment(line.trim(), assignment))
}

fn quoted_assignment(line: &str, assignment: &str) -> Option<String> {
    let rhs = line
        .strip_prefix(assignment)?
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let quote = rhs.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let rest = &rhs[1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn package_error(error: impl std::fmt::Display) -> fbuild_core::FbuildError {
    fbuild_core::FbuildError::PackageError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_core::platformio_package::{PackageSource, parse_package_spec};
    use fbuild_packages::{Package, Toolchain};

    const MANIFEST: &str = r#"{
      "frameworks": {"arduino": {"package": "framework-arduinoch32v003"}},
      "packages": {
        "toolchain-riscv": {"type": "toolchain", "owner": "platformio", "version": "https://github.com/Community-PIO-CH32V/toolchain-riscv-windows.git"},
        "framework-arduinoch32v003": {"type": "framework", "optional": true, "version": "https://github.com/Community-PIO-CH32V/arduino-wch32v003"},
        "framework-arduino-openwch-ch32": {"type": "framework", "optional": true, "version": "https://github.com/Community-PIO-CH32V/arduino_core_ch32.git"}
      }
    }"#;

    const BUILDER: &str = r##"
        gcc_branch = "#gcc12"
        FORCE_DOWNGRADE_TO_GCC8 = True
        if "arduino" in selected_frameworks or FORCE_DOWNGRADE_TO_GCC8:
            gcc_branch = ""
        if IS_LINUX:
            self.packages["toolchain-riscv"]["version"] = "https://github.com/Community-PIO-CH32V/toolchain-riscv-linux.git%s" % gcc_branch
        elif IS_MAC:
            self.packages["toolchain-riscv"]["version"] = "https://github.com/Community-PIO-CH32V/toolchain-riscv-mac.git%s" % gcc_branch
        else:
            self.packages["toolchain-riscv"]["version"] = "https://github.com/Community-PIO-CH32V/toolchain-riscv-windows.git%s" % gcc_branch
        if build_core == "ch32v003":
            self.frameworks["arduino"]["package"] = "framework-arduinoch32v003"
        elif build_core == "openwch":
            self.frameworks["arduino"]["package"] = "framework-arduino-openwch-ch32"
    "##;

    #[test]
    fn pinned_source_selects_openwch_and_linux_gcc8() {
        let selected =
            select_requirements(MANIFEST, BUILDER, "openwch", "linux_x86_64", &[]).unwrap();
        assert_eq!(selected.framework.name, "framework-arduino-openwch-ch32");
        assert_eq!(selected.toolchain.name, "toolchain-riscv");
        assert!(matches!(
            selected.toolchain.spec.source,
            PackageSource::Repository { ref url, reference: None }
                if url.ends_with("/toolchain-riscv-linux.git")
        ));
    }

    #[test]
    fn pinned_source_selects_host_specific_mac_and_windows_toolchains() {
        for (host, repository) in [
            ("darwin_x86_64", "toolchain-riscv-mac.git"),
            ("darwin_arm64", "toolchain-riscv-mac.git"),
            ("windows_amd64", "toolchain-riscv-windows.git"),
            ("windows_x86", "toolchain-riscv-windows.git"),
        ] {
            let selected = select_requirements(MANIFEST, BUILDER, "openwch", host, &[]).unwrap();
            assert!(
                matches!(
                    selected.toolchain.spec.source,
                    PackageSource::Repository { ref url, reference: None }
                        if url.ends_with(repository)
                ),
                "{host}"
            );
        }
    }

    #[test]
    fn explicit_toolchain_pin_wins_over_host_default() {
        let explicit = parse_package_spec("community-ch32v/toolchain-riscv@1.2.3").unwrap();
        let selected =
            select_requirements(MANIFEST, BUILDER, "openwch", "linux_x86_64", &[explicit]).unwrap();
        assert_eq!(
            selected
                .toolchain
                .spec
                .registry()
                .unwrap()
                .requirement
                .as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn explicit_framework_archive_wins_over_manifest_repository() {
        let url = "https://example.invalid/openwch-core.tar.gz";
        let explicit =
            parse_package_spec(&format!("framework-arduino-openwch-ch32@{url}")).unwrap();
        let selected =
            select_requirements(MANIFEST, BUILDER, "openwch", "linux_x86_64", &[explicit]).unwrap();
        assert!(matches!(
            selected.framework.spec.source,
            PackageSource::Archive { url: ref selected_url, revision: None }
                if selected_url == url
        ));
    }

    #[test]
    fn unsupported_host_does_not_fall_back_to_windows_toolchain() {
        let error =
            select_requirements(MANIFEST, BUILDER, "openwch", "linux_aarch64", &[]).unwrap_err();
        assert!(error.to_string().contains("linux_aarch64"));
    }

    #[test]
    fn only_bare_unpinned_ch32v_uses_native_defaults() {
        for (raw, expected) in [
            ("ch32v", true),
            ("ch32v@1.1.0", false),
            ("community-ch32v/ch32v", false),
            ("another-platform", false),
        ] {
            let spec = parse_package_spec(raw).unwrap();
            assert_eq!(
                is_legacy_default(spec.registry().unwrap()),
                expected,
                "{raw}"
            );
        }
    }

    #[test]
    fn selected_platform_board_core_and_variant_are_not_taken_from_builtin_board() {
        let selected = serde_json::json!({
            "core": "openwch",
            "variant": "GENERIC",
            "march": "rv32ecxw",
            "mabi": "ilp32e",
            "arduino": {"openwch": {
                "variant": "CH32V00x/CH32V003F4",
                "variant_h": "variant_CH32V003F4.h"
            }}
        });
        let board = select_board_build(&selected, &HashMap::new(), "stale_builtin_core").unwrap();
        assert_eq!(board.core, "openwch");
        assert_eq!(board.variant, "CH32V00x/CH32V003F4");
        assert_eq!(board.variant_h.as_deref(), Some("variant_CH32V003F4.h"));

        let env = HashMap::from([
            ("board_build.variant".into(), "custom_variant".into()),
            ("board_build.variant_h".into(), "custom.h".into()),
        ]);
        let overridden = select_board_build(&selected, &env, "stale_builtin_core").unwrap();
        assert_eq!(overridden.variant, "custom_variant");
        assert_eq!(overridden.variant_h.as_deref(), Some("custom.h"));
    }

    #[tokio::test]
    #[ignore = "downloads a pinned CH32V platform archive and resolves GitHub package refs"]
    async fn pinned_github_platform_resolves_real_ch32v_package_sources() {
        let project = tempfile::TempDir::new().unwrap();
        let env = HashMap::from([(
            "platform".to_string(),
            "https://github.com/Community-PIO-CH32V/platform-ch32v.git#b7397c29a71101175bfc94f6ab06f9daac336458".to_string(),
        ), ("board".to_string(), "genericCH32V003F4P6".to_string())]);
        let (toolchain, framework, board_isa, platform_resolution) =
            resolve_source_packages(project.path(), &env, "openwch", false)
                .await
                .unwrap()
                .unwrap();
        assert!(platform_resolution.contains("b7397c29a71101175bfc94f6ab06f9daac336458"));
        let toolchain_info = toolchain.get_info();
        let framework_info = framework.get_info();
        assert!(
            toolchain_info
                .url
                .contains("toolchain-riscv-linux/archive/")
        );
        assert!(!toolchain_info.url.contains("xpack"));
        assert!(framework_info.url.contains("arduino_core_ch32/archive/"));
        assert_eq!(board_isa.march, "rv32ecxw");
        let toolchain_root = toolchain.ensure_installed().await.unwrap();
        let framework_root = framework.ensure_installed().await.unwrap();
        assert!(toolchain_root.is_dir());
        assert!(framework_root.is_dir());
        assert!(toolchain.get_gcc_path().is_file());
        let gcc = toolchain.get_gcc_path();
        let gcc = gcc.to_str().expect("compiler path is UTF-8");
        let version = fbuild_core::subprocess::run_command(
            &[gcc, "--version"],
            None,
            None,
            Some(std::time::Duration::from_secs(10)),
        )
        .await
        .unwrap();
        assert!(version.success());
        assert!(version.stdout.contains("8.2.0"));
    }
}

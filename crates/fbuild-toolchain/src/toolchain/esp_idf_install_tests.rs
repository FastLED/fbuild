//! Unit + end-to-end tests for the whole-release ESP-IDF installer (#1970).

use super::*;
use crate::toolchain::esp_idf_release::IDF_RELEASE_VERSION;

fn fabricated_source(base: &Path) -> NormalizedPath {
    let root = NormalizedPath::new(base).join("source");
    std::fs::create_dir_all(root.join("tools").as_path()).unwrap();
    std::fs::write(root.join("tools").join("idf.py").as_path(), b"# idf\n").unwrap();
    root
}

fn fabricated_tool(base: &Path, component: IdfComponent, with_bin: bool) -> NormalizedPath {
    let root = NormalizedPath::new(base).join(component.id());
    std::fs::create_dir_all(root.as_path()).unwrap();
    if with_bin {
        let bin = root.join("bin");
        std::fs::create_dir_all(bin.as_path()).unwrap();
        std::fs::write(bin.join("tool").as_path(), b"x").unwrap();
    }
    root
}

fn fabricated_envs(base: &Path) -> (IdfEnvironment, IdfPythonEnvironment) {
    let tools = IdfEnvironment::from_roots(vec![
        (
            IdfComponent::XtensaEspElf,
            fabricated_tool(base, IdfComponent::XtensaEspElf, true),
        ),
        (
            IdfComponent::Esp32UlpElf,
            fabricated_tool(base, IdfComponent::Esp32UlpElf, true),
        ),
        (
            IdfComponent::EspRomElfs,
            fabricated_tool(base, IdfComponent::EspRomElfs, false),
        ),
        (
            IdfComponent::OpenocdEsp32,
            fabricated_tool(base, IdfComponent::OpenocdEsp32, true),
        ),
    ]);
    // OpenOCD ships its script tree; `env_vars` only exports a path that exists.
    let scripts = NormalizedPath::new(base)
        .join(IdfComponent::OpenocdEsp32.id())
        .join("share")
        .join("openocd")
        .join("scripts");
    std::fs::create_dir_all(scripts.as_path()).unwrap();
    let env_dir = NormalizedPath::new(base).join("python_env").join("idf_env");
    std::fs::create_dir_all(env_dir.join("bin").as_path()).unwrap();
    std::fs::write(env_dir.join("bin").join("python").as_path(), b"#!/bin/sh\n").unwrap();
    (tools, IdfPythonEnvironment::new(env_dir))
}

#[test]
fn the_venv_bin_leads_path_then_every_tool_bin() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (tools, python) = fabricated_envs(tmp.path());
    let installed =
        InstalledEspIdf::from_parts(fabricated_source(tmp.path()), tools, python.clone());
    let dirs = installed.path_dirs();
    assert_eq!(dirs.len(), 4, "venv bin + 3 tool bins: {dirs:?}");
    assert_eq!(
        dirs[0],
        python.bin_dir(),
        "a host python answering before the venv breaks idf.py's startup check"
    );
    assert_eq!(
        installed
            .tools()
            .tool_root(IdfComponent::XtensaEspElf)
            .unwrap()
            .join("bin"),
        dirs[1],
        "the profile's cross compiler must be first among the tools"
    );
}

#[test]
fn the_environment_names_idf_path_and_never_idf_tools_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (tools, python) = fabricated_envs(tmp.path());
    let source = fabricated_source(tmp.path());
    let installed = InstalledEspIdf::from_parts(source.clone(), tools, python.clone());
    let vars = installed.env_vars();
    let names: Vec<&str> = vars.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        [
            "IDF_PATH",
            "IDF_PYTHON_ENV_PATH",
            "ESP_ROM_ELF_DIR",
            "OPENOCD_SCRIPTS"
        ]
    );
    assert_eq!(vars[0].1, source);
    assert_eq!(vars[1].1, *python.env_dir());
    assert!(
        !names.contains(&"IDF_TOOLS_PATH"),
        "that variable would point idf_tools.py back at an ~/.espressif side tree"
    );
}

#[test]
fn installed_bytes_sums_every_unit_and_dedupes_hardlinks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = fabricated_source(tmp.path());
    let (tools, python) = fabricated_envs(tmp.path());
    // 4096 bytes in the source tree, of which 2048 are a hardlink of a file
    // inside the same tree: `du` counts it once.
    std::fs::write(source.join("a.bin").as_path(), vec![0u8; 2048]).unwrap();
    std::fs::hard_link(
        source.join("a.bin").as_path(),
        source.join("a-link.bin").as_path(),
    )
    .unwrap();
    let installed = InstalledEspIdf::from_parts(source, tools, python);
    let measured = installed.installed_bytes();
    // 2048 (hardlink counted once) + 6 (`tools/idf.py`) + 10 (venv python stub)
    // + 3 one-byte tool markers.
    assert_eq!(measured, 2067, "measured {measured} B");
    assert!(installed.missing_units().is_empty());
}

#[test]
fn a_missing_unit_is_named_instead_of_reading_as_a_small_tree() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = fabricated_source(tmp.path());
    let (tools, python) = fabricated_envs(tmp.path());
    let installed = InstalledEspIdf::from_parts(source, tools.clone(), python.clone());
    std::fs::remove_file(python.python().as_path()).unwrap();
    assert_eq!(installed.missing_units(), [IdfInstallUnit::Python]);
    let only_tools = InstalledEspIdf::from_parts(
        NormalizedPath::new(tmp.path()).join("nowhere"),
        tools,
        python,
    );
    assert_eq!(
        only_tools.missing_units(),
        [IdfInstallUnit::Source, IdfInstallUnit::Python]
    );
}

#[test]
fn an_empty_cache_reports_the_whole_release_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let install =
        EspIdfInstall::with_cache_root(tmp.path(), tmp.path(), IdfProfile::Esp32S3Only).unwrap();
    assert_eq!(install.profile(), IdfProfile::Esp32S3Only);
    assert_eq!(install.components().len(), 4);
    assert!(!install.is_installed());
    assert_eq!(install.installed_bytes(), 0);
    assert_eq!(
        IdfInstallUnit::ALL.map(IdfInstallUnit::id),
        ["source", "tools", "python"]
    );
}

/// The whole release, for real: 24 source archives, 4 tool archives and the
/// pinned interpreter with its locked wheels (~450 MB of downloads, ~1.6 GB on
/// disk). Asserts the combination, not the pieces: ESP-IDF's own
/// `tools/check_python_dependencies.py` runs out of the *installed* source tree
/// with the *installed* interpreter, which is the same gate `idf.py` runs at
/// startup, and the installed cross compiler reports the pinned version.
///
/// Run with:
/// `soldr cargo test -p fbuild-toolchain --lib -- --ignored esp_idf_release_install`
#[tokio::test]
#[ignore = "downloads the whole pinned ESP-IDF release (~450 MB) and installs ~1.6 GB"]
async fn esp_idf_release_install_provisions_the_whole_release() {
    let dir = tempfile::TempDir::new().unwrap();
    let install =
        EspIdfInstall::with_cache_root(dir.path(), dir.path(), IdfProfile::Esp32S3Only).unwrap();
    assert!(!install.is_installed());

    let installed = install.install().await.unwrap();
    assert!(install.is_installed());
    assert!(installed.missing_units().is_empty());

    // The tree is the pinned release, versioned by IDF's own CMake file.
    let version = installed
        .source_root()
        .join("tools")
        .join("cmake")
        .join("version.cmake");
    assert!(version.as_path().is_file(), "missing {version}");

    // The combination works: IDF's checker, installed python, installed
    // requirements, installed constraints.
    let checker = installed
        .source_root()
        .join("tools")
        .join("check_python_dependencies.py");
    let requirements = installed
        .source_root()
        .join("tools")
        .join("requirements")
        .join("requirements.core.txt");
    let constraints = install.python.constraints_path().to_string();
    let py = installed.python().python().to_string();
    let output = fbuild_core::subprocess::run_command(
        &[
            &py,
            &checker.to_string(),
            "-c",
            &constraints,
            "-r",
            &requirements.to_string(),
        ],
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        output.success(),
        "ESP-IDF rejected the combined environment:\n{}\n{}",
        output.stdout,
        output.stderr
    );
    assert!(
        output.stdout.contains("requirements are satisfied"),
        "unexpected checker output: {}",
        output.stdout
    );

    // And the toolchain half is real, at the pinned version.
    let gcc = installed
        .tools()
        .tool_root(IdfComponent::XtensaEspElf)
        .unwrap()
        .join("bin")
        .join("xtensa-esp32-elf-gcc");
    let version_out =
        fbuild_core::subprocess::run_command(&[&gcc.to_string(), "--version"], None, None, None)
            .await
            .unwrap();
    assert!(
        version_out.stdout.contains("esp-14.2.0_20241119"),
        "xtensa GCC is not the pinned release: {}",
        version_out.stdout
    );

    // Report, then AC2's tools-only budget plus the aggregate total.
    let tools_bytes = install.release.installed_bytes();
    let total = installed.installed_bytes();
    println!(
        "{IDF_RELEASE_VERSION}: tools {tools_bytes} B, release total {total} B ({:.0} MB)",
        total as f64 / 1e6
    );
    assert!(
        tools_bytes <= 1_300_000_000,
        "tools measured {tools_bytes} B"
    );
    assert!(
        total > 1_500_000_000,
        "aggregate measurement looks truncated: {total} B"
    );

    // AC4: a second install of the whole release is a no-op.
    const SENTINEL: &[u8] = b"keep me";
    let sentinel = installed.source_root().join("agent-sentinel");
    std::fs::write(sentinel.as_path(), SENTINEL).unwrap();
    let again = install.install().await.unwrap();
    assert!(
        sentinel.as_path().is_file(),
        "a second install re-extracted"
    );
    assert_eq!(again.env_vars(), installed.env_vars());
    assert_eq!(again.path_dirs(), installed.path_dirs());
    assert_eq!(
        again.installed_bytes(),
        total + SENTINEL.len() as u64,
        "a second install changed more than the sentinel"
    );
}

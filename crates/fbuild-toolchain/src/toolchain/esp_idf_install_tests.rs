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
        (
            IdfComponent::Cmake,
            fabricated_tool(base, IdfComponent::Cmake, true),
        ),
        // Ninja's archive is a single binary at its root: no `bin/`, and the
        // root itself is the PATH entry.
        (
            IdfComponent::Ninja,
            fabricated_tool(base, IdfComponent::Ninja, false),
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
    assert_eq!(dirs.len(), 6, "venv bin + 5 tool dirs: {dirs:?}");
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
fn the_environment_names_idf_path_and_points_idf_tools_path_into_our_own_tree() {
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
            "IDF_TOOLS_PATH",
            "ESP_ROM_ELF_DIR",
            "OPENOCD_SCRIPTS"
        ]
    );
    assert_eq!(vars[0].1, source);
    assert_eq!(vars[1].1, *python.env_dir());
    // `idf_tools.py` looks for `<IDF_TOOLS_PATH>/espidf.constraints.v5.5.txt`
    // and aborts without it, so the variable has to name the directory that
    // holds our pinned constraints -- and that directory must be inside the
    // cache entry, never the user's `~/.espressif`.
    assert_eq!(vars[2].1, python.python_env_dir());
    assert_eq!(
        vars[2].1.join("espidf.constraints.v5.5.txt"),
        python
            .env_dir()
            .as_path()
            .parent()
            .map(|p| NormalizedPath::new(p).join("espidf.constraints.v5.5.txt"))
            .unwrap()
    );
    assert!(
        !vars[2].1.as_path().ends_with(".espressif"),
        "IDF_TOOLS_PATH must not be the home side tree"
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
    // + 4 one-byte tool markers (three toolchain `bin`s + CMake's; Ninja's
    // fabrication has no `bin/`, and a `PATH`-only flat root adds no bytes).
    assert_eq!(measured, 2068, "measured {measured} B");
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
    assert_eq!(install.components().len(), 6);
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
    let (dir, cache_root, persistent) = proof_cache();
    let install =
        EspIdfInstall::with_cache_root(dir.path(), &cache_root, IdfProfile::Esp32S3Only).unwrap();
    // Only a cold cache can prove "not installed to start with"; a reused proof
    // cache starts warm, and the no-op half of the test still runs.
    if persistent.is_none() {
        assert!(!install.is_installed());
    }

    let installed = install.install().await.unwrap();
    assert!(install.is_installed());
    assert!(installed.missing_units().is_empty());
    assert_source_tools_and_python_are_the_pinned_release(&installed, &install).await;
    assert_configure_with_the_pinned_build_tools(&installed).await;
    report_budgets(&install, &installed);
    assert_a_second_install_is_a_no_op(&install, &installed).await;
}

/// Cache root for the proof: a fresh temp dir, or `FBUILD_IDF_PROOF_CACHE`
/// when set, so iterating on the *build* half does not repay ~500 MB of
/// downloads and a 1.7 GB extract on every attempt.
fn proof_cache() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Option<std::path::PathBuf>,
) {
    let persistent = std::env::var_os("FBUILD_IDF_PROOF_CACHE").map(std::path::PathBuf::from);
    let dir = match &persistent {
        Some(path) => tempfile::TempDir::new_in(path).unwrap(),
        None => tempfile::TempDir::new().unwrap(),
    };
    let cache_root = match &persistent {
        Some(path) => path.clone(),
        None => dir.path().to_path_buf(),
    };
    (dir, cache_root, persistent)
}

/// The three installed units are the pinned release, and they agree with each
/// other: IDF's own version file, IDF's own dependency checker running out of
/// the installed tree with the installed interpreter, and the pinned compiler.
async fn assert_source_tools_and_python_are_the_pinned_release(
    installed: &InstalledEspIdf,
    install: &EspIdfInstall,
) {
    let version = installed
        .source_root()
        .join("tools")
        .join("cmake")
        .join("version.cmake");
    assert!(version.as_path().is_file(), "missing {version}");

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
}

/// The whole point of the build tools: ESP-IDF's OWN launcher starts in the
/// installed release, and a real `set-target` configure runs CMake, Ninja and
/// the pinned cross compiler -- the first half of the delegated build #1971
/// adds. `idf.py` runs its startup checks here (cmake/ninja presence,
/// constraints file, python env), so a missing piece fails here instead of
/// halfway through a build.
async fn assert_configure_with_the_pinned_build_tools(installed: &InstalledEspIdf) {
    let idf_py = installed.source_root().join("tools").join("idf.py");
    let idf_version = run_in_installed_env(installed, &idf_py, &["--version"], None).await;
    assert!(
        idf_version.success() && idf_version.stdout.contains(IDF_RELEASE_VERSION),
        "idf.py did not start in the installed release:\n{}\n{}",
        idf_version.stdout,
        idf_version.stderr
    );

    let project = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("main")).unwrap();
    std::fs::write(
        project.path().join("CMakeLists.txt"),
        b"cmake_minimum_required(VERSION 3.16)\ninclude($ENV{IDF_PATH}/tools/cmake/project.cmake)\nproject(agent_probe)\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("main").join("CMakeLists.txt"),
        b"idf_component_register(SRCS \"main.c\")\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("main").join("main.c"),
        b"void app_main(void) {}\n",
    )
    .unwrap();

    let configured = run_in_installed_env(
        installed,
        &idf_py,
        &["set-target", "esp32s3"],
        Some(project.path()),
    )
    .await;
    assert!(
        configured.success(),
        "idf.py set-target esp32s3 failed against the installed release:\n{}\n{}",
        configured.stdout,
        configured.stderr
    );

    let cache = project.path().join("build").join("CMakeCache.txt");
    assert!(
        cache.as_path().is_file(),
        "no CMakeCache.txt in the project"
    );
    let cache_text = std::fs::read_to_string(cache.as_path()).unwrap();
    // The configure used OUR pins, not the host's.
    for expected in [
        "CMAKE_GENERATOR:INTERNAL=Ninja",
        "CMAKE_MAKE_PROGRAM:FILEPATH=",
        "xtensa-esp32s3-elf-gcc",
    ] {
        assert!(
            cache_text.contains(expected),
            "configured project does not mention {expected}:\n{cache_text}"
        );
    }
    let pinned_ninja = installed
        .tools()
        .tool_root(IdfComponent::Ninja)
        .expect("the profile installed Ninja")
        .as_path()
        .to_string_lossy()
        .into_owned();
    let make_program_line = cache_text
        .lines()
        .find(|line| line.starts_with("CMAKE_MAKE_PROGRAM:FILEPATH="))
        .expect("no CMAKE_MAKE_PROGRAM in the cache");
    assert!(
        make_program_line.contains(&pinned_ninja),
        "the configure used a host Ninja instead of the pinned one: {make_program_line}"
    );
}

/// Print the three measurements and check both budgets. Returns the aggregate
/// total, which the no-op check compares against.
fn report_budgets(install: &EspIdfInstall, installed: &InstalledEspIdf) -> u64 {
    let toolchains = install.release.target_toolchain_bytes();
    let build_tools = install.release.build_tool_bytes();
    let total = installed.installed_bytes();
    println!(
        "{IDF_RELEASE_VERSION}: target toolchains {toolchains} B, build tools {build_tools} B, release total {total} B ({:.0} MB)",
        total as f64 / 1e6
    );
    assert!(
        toolchains <= 1_300_000_000,
        "target toolchains measured {toolchains} B"
    );
    assert!(
        build_tools <= 200_000_000,
        "build tools measured {build_tools} B"
    );
    assert!(
        total > 1_500_000_000,
        "aggregate measurement looks truncated: {total} B"
    );
    total
}

/// AC4: installing again re-downloads and re-extracts nothing.
///
/// Measured as a delta from the sentinel-carrying tree rather than from the
/// previous total: with `FBUILD_IDF_PROOF_CACHE` the sentinel of an earlier run
/// is already there, and the property under test is that a re-install changes
/// nothing -- not that this is the first run.
async fn assert_a_second_install_is_a_no_op(install: &EspIdfInstall, installed: &InstalledEspIdf) {
    const SENTINEL: &[u8] = b"keep me";
    let sentinel = installed.source_root().join("agent-sentinel");
    std::fs::write(sentinel.as_path(), SENTINEL).unwrap();
    let with_sentinel = installed.installed_bytes();
    let again = install.install().await.unwrap();
    assert!(
        sentinel.as_path().is_file(),
        "a second install re-extracted"
    );
    assert_eq!(again.env_vars(), installed.env_vars());
    assert_eq!(again.path_dirs(), installed.path_dirs());
    assert_eq!(
        again.installed_bytes(),
        with_sentinel,
        "a second install changed the installed tree"
    );
}

/// Run `program` inside the environment [`InstalledEspIdf`] describes: the
/// profile's `PATH` (venv first), `IDF_PATH`, and the CMake-facing variables.
///
/// Deliberately not a shell string: the environment is exactly what
/// [`InstalledEspIdf::path_dirs`] and [`InstalledEspIdf::env_vars`] produce, so
/// the proof fails if either half of the answer is wrong.
async fn run_in_installed_env(
    installed: &InstalledEspIdf,
    program: &NormalizedPath,
    args: &[&str],
    cwd: Option<&Path>,
) -> fbuild_core::subprocess::ToolOutput {
    // The profile's directories are PREPENDED to the host `PATH`, never
    // substituted for it: `idf.py` and CMake shell out to `ln`, `git` and
    // friends, so a PATH holding only the pinned tool dirs makes CMake fail
    // deep inside mbedtls with "Could not create symbolic link" -- measured
    // 2026-11-10. A real invocation starts from the user's shell PATH.
    let mut entries: Vec<std::path::PathBuf> = installed
        .path_dirs()
        .iter()
        .map(|dir| dir.as_path().to_path_buf())
        .collect();
    if let Some(host_path) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&host_path));
    }
    let path = std::env::join_paths(entries).expect("PATH entries must be joinable");
    let mut owned: Vec<(&str, String)> = vec![("PATH", path.to_string_lossy().into_owned())];
    for (name, value) in installed.env_vars() {
        owned.push((name, value.as_path().to_string_lossy().into_owned()));
    }
    let refs: Vec<(&str, &str)> = owned
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let mut argv_owned: Vec<String> = vec![program.as_path().to_string_lossy().into_owned()];
    argv_owned.extend(args.iter().map(|arg| arg.to_string()));
    let argv: Vec<&str> = argv_owned.iter().map(|arg| arg.as_str()).collect();
    fbuild_core::subprocess::run_command(&argv, cwd, Some(&refs), None)
        .await
        .expect("failed to spawn the installed release's tool")
}

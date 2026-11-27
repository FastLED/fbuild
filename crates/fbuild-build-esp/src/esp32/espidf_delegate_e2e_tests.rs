//! End-to-end proof for the delegated ESP-IDF build (FastLED/fbuild#1971).
//!
//! One ignored test, gated on a locally installed ESP-IDF: it drives the REAL
//! orchestrator entry point over `tests/platform/espidf_component`, so the
//! opt-in, the pinned release's environment, `idf.py set-target`, `idf.py
//! build` and artifact publication are all exercised as production runs them.
//!
//! Run with:
//!
//! ```text
//! soldr cargo test -p fbuild-build-esp --lib -- --ignored espidf_component_project
//! ```
//!
//! A first run installs the pinned release (~1.8 GB). Point
//! `FBUILD_IDF_PROOF_CACHE` at a directory to keep that install between
//! attempts; the test then only repays the build.

use std::path::Path;

use fbuild_core::path::NormalizedPath;

use crate::BuildOrchestrator;

/// Copy a committed `tests/platform/<fixture>` into a scratch project so the
/// build never writes into the repository.
fn copy_fixture(fixture: &str, destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("platform")
        .join(fixture);
    std::fs::create_dir_all(destination).unwrap();
    let source = NormalizedPath::new(&source);
    let destination = NormalizedPath::new(destination);
    for entry in walkdir::WalkDir::new(source.as_path()) {
        let entry = entry.unwrap();
        let relative = NormalizedPath::new(entry.path())
            .relative_to(&source)
            .unwrap();
        let target = destination.join(relative.as_path());
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(target.as_path()).unwrap();
        } else {
            std::fs::copy(entry.path(), target.as_path()).unwrap();
        }
    }
}

/// Root the build runs in: a fresh temp dir, or `FBUILD_IDF_PROOF_CACHE` when
/// set so the ~1.8 GB install survives between attempts.
fn build_root() -> (tempfile::TempDir, NormalizedPath) {
    match std::env::var_os("FBUILD_IDF_PROOF_CACHE") {
        Some(value) => {
            let root = NormalizedPath::new(Path::new(&value));
            std::fs::create_dir_all(root.as_path()).unwrap();
            // The whole fbuild cache follows this variable, which is what makes
            // the install reusable and keeps it out of the user's real cache.
            std::env::set_var("FBUILD_CACHE_DIR", root.as_path());
            let scratch = tempfile::TempDir::new_in(root.as_path()).unwrap();
            (scratch, root)
        }
        None => {
            let scratch = tempfile::TempDir::new().unwrap();
            let root = NormalizedPath::new(scratch.path());
            (scratch, root)
        }
    }
}

fn params(
    project: &NormalizedPath,
    build_dir: &NormalizedPath,
    env_name: &str,
) -> crate::BuildParams {
    crate::BuildParams {
        prebuilt_libs: Default::default(),
        project_dir: project.as_path().to_path_buf(),
        shrink: Default::default(),
        env_name: env_name.into(),
        clean_all: false,
        clean_only: false,
        clean: false,
        profile: fbuild_core::BuildProfile::Release,
        build_dir: build_dir.as_path().to_path_buf(),
        verbose: false,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: true,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
        caller_path: None,
    }
}

/// A real component project builds through fbuild's ESP32 orchestrator and
/// produces every artifact #1971 names, under `<build>/idf/`.
#[tokio::test]
#[ignore = "installs the pinned ESP-IDF release (~1.8 GB) and runs a real idf.py build"]
async fn espidf_component_project_builds_and_publishes_artifacts() {
    let (_scratch, root) = build_root();
    let project = root.join("project");
    copy_fixture("espidf_component", project.as_path());
    let build_dir = root.join("build");

    let result = crate::esp32::Esp32Orchestrator
        .build(&params(&project, &build_dir, "esp32s3"))
        .await
        .expect("the delegated build must succeed");

    assert!(result.success, "{}", result.message);
    assert!(result.build_time_secs > 0.0);
    let idf_dir = build_dir.join("idf");

    for name in [
        "component_demo.bin",
        "component_demo.elf",
        "component_demo.map",
    ] {
        let path = idf_dir.join(name);
        assert!(path.as_path().is_file(), "missing artifact {path}");
    }
    for name in [
        "bootloader.bin",
        "partition-table.bin",
        "sdkconfig",
        "compile_commands.json",
        "idf-artifacts.json",
    ] {
        let path = idf_dir.join(name);
        assert!(path.as_path().is_file(), "missing artifact {path}");
    }

    // The ELF is what a debugger loads; the compile database is what #1973
    // re-roots. Both must be non-trivial, not placeholders.
    let elf = idf_dir.join("component_demo.elf");
    assert!(
        std::fs::metadata(elf.as_path()).unwrap().len() > 1_000_000,
        "{elf} looks too small to be a linked image"
    );
    // #1973: the database fbuild publishes is the re-rooted, merged one at the
    // documented project path; `idf.py`'s own copy under `<build>/idf/` stays
    // behind as the delegate's raw output.
    let compiledb = project.join("compile_commands.json");
    let entries: serde_json::Value =
        serde_json::from_slice(&std::fs::read(compiledb.as_path()).unwrap()).unwrap();
    let entries = entries
        .as_array()
        .expect("compile_commands.json is an array");
    assert!(
        entries.len() > 100,
        "only {} compile commands: the IDF graph did not build",
        entries.len()
    );
    let project_spelling = project.display_slash();
    for entry in entries {
        assert_eq!(
            entry["directory"], project_spelling,
            "an entry is still rooted in the build directory: {entry}"
        );
        let file = entry["file"].as_str().unwrap_or_default();
        assert!(
            !file.starts_with(&idf_dir.display_slash())
                && !file.starts_with(&build_dir.display_slash()),
            "a scratch prefix survived in {file}"
        );
    }

    // The index is the artifact proof the follow-ups consume.
    let index: serde_json::Value = serde_json::from_slice(
        &std::fs::read(idf_dir.join("idf-artifacts.json").as_path()).unwrap(),
    )
    .unwrap();
    assert_eq!(index["project"], "component_demo");
    assert_eq!(index["chip"], "esp32s3");
    assert_eq!(index["files"].as_array().unwrap().len(), 7);

    // The build result points at the published image, not at IDF's tree.
    assert_eq!(
        result.firmware_path.as_deref(),
        Some(idf_dir.join("component_demo.bin").as_path())
    );
    assert_eq!(
        result.compile_database_path.as_deref(),
        Some(compiledb.as_path())
    );
}

/// Build one environment of `tests/platform/espidf_component_riscv` through the
/// real orchestrator and check the artifacts the delegated build publishes.
///
/// Each chip gets its own project copy and build directory under the shared
/// root, so the tests can run one after another against one install.
async fn build_riscv_component_project(env_name: &str, chip: &str) {
    let (_scratch, root) = build_root();
    let project = root.join(format!("project-{env_name}"));
    copy_fixture("espidf_component_riscv", project.as_path());
    let build_dir = root.join(format!("build-{env_name}"));

    let result = crate::esp32::Esp32Orchestrator
        .build(&params(&project, &build_dir, env_name))
        .await
        .expect("the delegated build must succeed");

    assert!(result.success, "{}", result.message);
    let idf_dir = build_dir.join("idf");
    for name in [
        "riscv_component_demo.bin",
        "riscv_component_demo.elf",
        "riscv_component_demo.map",
        "bootloader.bin",
        "partition-table.bin",
        "sdkconfig",
        "compile_commands.json",
        "idf-artifacts.json",
    ] {
        let path = idf_dir.join(name);
        assert!(path.as_path().is_file(), "missing artifact {path}");
    }

    // The target is the board's chip, not a value the project hard-codes: the
    // fixture's `sdkconfig.defaults` names no `CONFIG_IDF_TARGET`.
    let sdkconfig = std::fs::read_to_string(idf_dir.join("sdkconfig").as_path()).unwrap();
    assert!(
        sdkconfig.contains(&format!("CONFIG_IDF_TARGET=\"{chip}\"")),
        "the generated sdkconfig does not target {chip}"
    );
    // `sdkconfig.defaults` reached the build (`main.c` also refuses to compile
    // otherwise).
    assert!(sdkconfig.contains("CONFIG_LOG_DEFAULT_LEVEL_WARN=y"));

    let index: serde_json::Value = serde_json::from_slice(
        &std::fs::read(idf_dir.join("idf-artifacts.json").as_path()).unwrap(),
    )
    .unwrap();
    assert_eq!(index["project"], "riscv_component_demo");
    assert_eq!(index["chip"], chip);

    // The RISC-V compiler built it: every project entry of the published
    // database is compiled by `riscv32-esp-elf-*`, never by the Xtensa GCC.
    let compiledb = project.join("compile_commands.json");
    let entries: serde_json::Value =
        serde_json::from_slice(&std::fs::read(compiledb.as_path()).unwrap()).unwrap();
    let entries = entries
        .as_array()
        .expect("compile_commands.json is an array");
    assert!(
        entries.len() > 100,
        "only {} compile commands",
        entries.len()
    );
    let probe = entries
        .iter()
        .find(|entry| {
            entry["file"]
                .as_str()
                .is_some_and(|file| file.ends_with("probe.c"))
        })
        .expect("the project's own component was compiled");
    let invocation = match probe["command"].as_str() {
        Some(command) => command.to_string(),
        None => probe["arguments"].to_string(),
    };
    assert!(
        invocation.contains("riscv32-esp-elf-"),
        "probe.c was not compiled by the RISC-V toolchain: {invocation}"
    );
}

#[tokio::test]
#[ignore = "installs the pinned ESP-IDF release with the RISC-V toolchain (~2.8 GB) and runs a real idf.py build"]
async fn espidf_riscv_component_project_builds_for_esp32c5() {
    build_riscv_component_project("esp32c5", "esp32c5").await;
}

#[tokio::test]
#[ignore = "installs the pinned ESP-IDF release with the RISC-V toolchain (~2.8 GB) and runs a real idf.py build"]
async fn espidf_riscv_component_project_builds_for_esp32p4() {
    build_riscv_component_project("esp32p4", "esp32p4").await;
}

#[tokio::test]
#[ignore = "installs the pinned ESP-IDF release with the RISC-V toolchain (~2.8 GB) and runs a real idf.py build"]
async fn espidf_riscv_component_project_builds_for_esp32c3() {
    build_riscv_component_project("esp32c3", "esp32c3").await;
}

#[tokio::test]
#[ignore = "installs the pinned ESP-IDF release with the RISC-V toolchain (~2.8 GB) and runs a real idf.py build"]
async fn espidf_riscv_component_project_builds_for_esp32c6() {
    build_riscv_component_project("esp32c6", "esp32c6").await;
}

/// ESP32-P4 rev-3 silicon builds on the moved pin, and the image is built for
/// the rev-3 family (`CONFIG_ESP32P4_REV_MIN_FULL=300`); the pre-rev-3 board
/// keeps its own family on the same release (FastLED/fbuild#2133).
#[tokio::test]
#[ignore = "installs the pinned ESP-IDF release with the RISC-V toolchain (~2.8 GB) and runs two real idf.py builds"]
async fn espidf_riscv_component_project_builds_for_both_p4_silicon_families() {
    let (_scratch, root) = build_root();

    let project = root.join("project-esp32p4-r3");
    copy_fixture("espidf_component_riscv", project.as_path());
    let build_dir = root.join("build-esp32p4-r3");
    let result = crate::esp32::Esp32Orchestrator
        .build(&params(&project, &build_dir, "esp32p4-r3"))
        .await
        .expect("rev-3 silicon must build on the pinned release");
    assert!(result.success, "{}", result.message);
    let sdkconfig =
        std::fs::read_to_string(build_dir.join("idf").join("sdkconfig").as_path()).unwrap();
    assert!(
        sdkconfig.contains("CONFIG_ESP32P4_REV_MIN_FULL=300"),
        "the rev-3 family was not selected:\n{sdkconfig}"
    );
    assert!(
        !sdkconfig.contains("CONFIG_ESP32P4_SELECTS_REV_LESS_V3=y"),
        "the pre-rev-3 family was selected for a rev-3 board:\n{sdkconfig}"
    );

    // The pre-rev-3 board keeps its own family: one release, both families.
    let pre = root.join("project-esp32p4");
    copy_fixture("espidf_component_riscv", pre.as_path());
    let pre_build = root.join("build-esp32p4");
    let result = crate::esp32::Esp32Orchestrator
        .build(&params(&pre, &pre_build, "esp32p4"))
        .await
        .expect("pre-rev-3 silicon must keep building");
    assert!(result.success, "{}", result.message);
    let sdkconfig =
        std::fs::read_to_string(pre_build.join("idf").join("sdkconfig").as_path()).unwrap();
    assert!(
        sdkconfig.contains("CONFIG_ESP32P4_SELECTS_REV_LESS_V3=y"),
        "the pre-rev-3 family was not selected:\n{sdkconfig}"
    );
    assert!(
        sdkconfig.contains("CONFIG_ESP32P4_REV_MIN_FULL=0"),
        "the pre-rev-3 revision floor is wrong:\n{sdkconfig}"
    );
}

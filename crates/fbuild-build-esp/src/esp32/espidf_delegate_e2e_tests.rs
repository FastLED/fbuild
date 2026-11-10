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

/// Copy the committed fixture into a scratch project so the build never writes
/// into the repository.
fn copy_fixture(destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("platform")
        .join("espidf_component");
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

fn params(project: &NormalizedPath, build_dir: &NormalizedPath) -> crate::BuildParams {
    crate::BuildParams {
        project_dir: project.as_path().to_path_buf(),
        shrink: Default::default(),
        env_name: "esp32s3".into(),
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
    copy_fixture(project.as_path());
    let build_dir = root.join("build");

    let result = crate::esp32::Esp32Orchestrator
        .build(&params(&project, &build_dir))
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
    let compiledb = idf_dir.join("compile_commands.json");
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

//! Verifies fbuild's nxplpc compile command shape against ArduinoCore-LPC8xx.

use fbuild_build::{BuildOrchestrator, BuildParams};
use fbuild_core::BuildProfile;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn arduino_core_repo() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))?;
    let repo = home.join("dev").join("ArduinoCore-LPC8xx");
    repo.join("platformio.ini").is_file().then_some(repo)
}

fn build_core_repo(repo: &Path, env_name: &str) -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let build_dir = tmp
        .path()
        .join(".fbuild/build")
        .join(env_name)
        .join("release");

    let params = BuildParams {
        project_dir: repo.to_path_buf(),
        env_name: env_name.to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
        generate_compiledb: true,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
    };

    let orchestrator = fbuild_build::nxplpc::orchestrator::NxpLpcOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("ArduinoCore-LPC8xx nxplpc build should succeed");
    assert!(result.success);
    tmp
}

#[test]
#[ignore = "requires local ~/dev/ArduinoCore-LPC8xx checkout and ARM toolchain package"]
fn arduino_core_lpc845brk_compile_commands_match_platform_txt() {
    let Some(repo) = arduino_core_repo() else {
        eprintln!("skipping: ~/dev/ArduinoCore-LPC8xx not found");
        return;
    };
    let tmp = build_core_repo(&repo, "lpc845brk");
    let compile_db = tmp
        .path()
        .join(".fbuild/build/lpc845brk/release/compile_commands.json");
    let text = fs::read_to_string(&compile_db).expect("compile_commands.json");
    let entries: Vec<Value> = serde_json::from_str(&text).expect("valid compile database");
    let args = entries
        .first()
        .and_then(|entry| entry.get("arguments"))
        .and_then(Value::as_array)
        .expect("first compile command has arguments");

    let has = |needle: &str| args.iter().any(|arg| arg.as_str() == Some(needle));
    assert!(has("-std=gnu++11"));
    assert!(has("-fno-use-cxa-atexit"));
    assert!(!has("-std=gnu++17"));
    assert!(!args.iter().any(|arg| {
        arg.as_str()
            .is_some_and(|arg| arg == "-flto" || arg.starts_with("-flto="))
    }));
    assert!(!args.iter().any(|arg| {
        arg.as_str()
            .is_some_and(|arg| arg.starts_with("-mfloat-abi"))
    }));
    assert!(!has("-nostartfiles"));
}

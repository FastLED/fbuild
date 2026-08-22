//! Verifies fbuild's nxplpc compile command shape against ArduinoCore-LPC8xx.

use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
use fbuild_core::BuildProfile;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// 15-min wall-clock cap for `--ignored` real-toolchain tests (FastLED/fbuild#806).
const REAL_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

async fn under_test_timeout<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::time::timeout(REAL_BUILD_TIMEOUT, fut).await {
        Ok(v) => v,
        Err(_) => panic!(
            "real-toolchain test exceeded {:.0}s budget — see FastLED/fbuild#806",
            REAL_BUILD_TIMEOUT.as_secs_f64()
        ),
    }
}

/// The orchestrator compiles through the process-wide compile backend
/// (FastLED/fbuild#800), which only the daemon wires at startup — this
/// integration test must install its own.
///
/// Initialized at most once per test process: a second concurrent
/// `CompileBackend::start()` cannot win the zccache cache-root writer slot
/// from the first and fails with "another live daemon already holds this
/// cache root".
async fn install_test_compile_backend() {
    static INSTALL: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    INSTALL
        .get_or_init(|| async {
            let backend = compile_backend::CompileBackend::start()
                .await
                .expect("compile backend starts for nxp lpc compile-commands test");
            compile_backend::install_global(backend);
        })
        .await;
}

fn arduino_core_repo() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))?;
    let repo = home.join("dev").join("ArduinoCore-LPC8xx");
    repo.join("platformio.ini").is_file().then_some(repo)
}

async fn build_core_repo(repo: &Path, env_name: &str) -> tempfile::TempDir {
    install_test_compile_backend().await;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let build_dir = tmp
        .path()
        .join(".fbuild/build")
        .join(env_name)
        .join("release");

    let params = BuildParams {
        project_dir: repo.to_path_buf(),
        env_name: env_name.to_string(),
        clean_all: false,
        clean_only: false,
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
        caller_path: None,
    };

    let orchestrator = fbuild_build::nxplpc::orchestrator::NxpLpcOrchestrator;
    let result = under_test_timeout(orchestrator.build(&params))
        .await
        .expect("ArduinoCore-LPC8xx nxplpc build should succeed");
    assert!(result.success);
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires local ~/dev/ArduinoCore-LPC8xx checkout and ARM toolchain package"]
async fn arduino_core_lpc845brk_compile_commands_match_platform_txt() {
    let Some(repo) = arduino_core_repo() else {
        eprintln!("skipping: ~/dev/ArduinoCore-LPC8xx not found");
        return;
    };
    let tmp = build_core_repo(&repo, "lpc845brk").await;
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

//! `fbuild build` handler and a couple of path / browser helpers.

use crate::daemon_client::{self, BuildRequest, DaemonClient};
use crate::output;

pub async fn open_in_browser(url: &str) -> fbuild_core::Result<()> {
    let args: Vec<&str> = if cfg!(target_os = "windows") {
        vec!["cmd", "/c", "start", "", url]
    } else if cfg!(target_os = "macos") {
        vec!["open", url]
    } else {
        vec!["xdg-open", url]
    };
    // FastLED/fbuild#810: cap the browser launcher at 10s — `start` / `open` /
    // `xdg-open` return effectively instantly in practice, but we shouldn't
    // let a wedged shell hang `--launch-browser` indefinitely.
    let output = fbuild_core::subprocess::run_command(
        &args,
        None,
        None,
        Some(std::time::Duration::from_secs(10)),
    )
    .await
    .map_err(|e| fbuild_core::FbuildError::Other(format!("failed to launch browser: {}", e)))?;

    if output.success() {
        Ok(())
    } else {
        Err(fbuild_core::FbuildError::Other(format!(
            "browser launcher exited with status {}",
            output.exit_code
        )))
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_build(
    project_dir: String,
    environment: Option<String>,
    clean: bool,
    clean_all: bool,
    verbose: bool,
    jobs: Option<usize>,
    quick: bool,
    release: bool,
    dry_run: bool,
    target: Option<String>,
    symbol_analysis: Option<String>,
    no_timestamp: bool,
    output_dir: Option<String>,
    bloat_analysis: bool,
) -> fbuild_core::Result<()> {
    // FBUILD_PERF_LOG=1 enables coarse CLI-side timing (daemon handshake +
    // round-trip). Zero-overhead when unset.
    let perf_enabled = std::env::var("FBUILD_PERF_LOG")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    let cli_start = std::time::Instant::now();
    let handshake_start = std::time::Instant::now();
    daemon_client::ensure_daemon_running().await?;
    let handshake_elapsed = handshake_start.elapsed();

    // Dry-run: verify daemon starts and environment resolves, then exit
    if dry_run {
        let project_path = std::path::Path::new(&project_dir);
        let ini_path = project_path.join("platformio.ini");
        let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
        let env_name = environment
            .as_deref()
            .or_else(|| config.get_default_environment())
            .unwrap_or("default");
        output::result(format!("Environment: {}", env_name));
        output::result("Daemon is running. Dry-run complete.");
        return Ok(());
    }

    let client = DaemonClient::new();
    daemon_client::warn_if_daemon_identity_mismatch(&client, &project_dir).await;
    if verbose {
        output::debug(daemon_client::runtime_diagnostic());
    }

    let profile = if release {
        Some("release".to_string())
    } else if quick {
        Some("quick".to_string())
    } else {
        None
    };
    let generate_compiledb = target.as_deref() == Some("compiledb");
    if generate_compiledb {
        let env_label = environment.as_deref().unwrap_or("default");
        output::progress(format!(
            "Generating compile_commands.json for environment: {}...",
            env_label
        ));
    }
    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let req = BuildRequest {
        project_dir: project_dir.clone(),
        environment,
        clean_build: clean || clean_all,
        clean_all,
        clean_only: false,
        verbose,
        jobs,
        profile,
        generate_compiledb,
        compiledb_only: generate_compiledb,
        request_id: None,
        caller_pid,
        caller_cwd,
        stream: true,
        symbol_analysis: symbol_analysis.is_some(),
        symbol_analysis_path: symbol_analysis.filter(|s| !s.is_empty()),
        no_timestamp,
        src_dir: std::env::var("PLATFORMIO_SRC_DIR")
            .ok()
            .filter(|s| !s.is_empty()),
        output_dir,
        pio_env: daemon_client::capture_pio_env(),
        bloat_analysis,
    };

    let stream_start = std::time::Instant::now();
    let resp = client.build_streaming(&req).await?;
    let stream_elapsed = stream_start.elapsed();
    if !resp.message.is_empty() {
        output::result(&resp.message);
    }
    if perf_enabled {
        let summary = format!(
            "[perf-log cli-build] daemon-handshake={} ms, server-roundtrip={} ms, total={} ms",
            handshake_elapsed.as_millis(),
            stream_elapsed.as_millis(),
            cli_start.elapsed().as_millis(),
        );
        tracing::info!(target: "fbuild_cli::perf_log", "{}", summary);
        output::debug(&summary);
    }
    if !resp.success {
        if !verbose {
            output::error(daemon_client::runtime_diagnostic());
        }
        std::process::exit(resp.exit_code);
    }
    if generate_compiledb {
        let db_path = std::path::Path::new(&project_dir).join("compile_commands.json");
        if db_path.exists() {
            output::result(format!(
                "compile_commands.json written to {}",
                db_path.display()
            ));
        }
    }
    Ok(())
}

/// Convert MSYS/Git-Bash paths (/c/Users/...) to native Windows paths and canonicalize.
pub async fn normalize_path(path: &str) -> fbuild_core::Result<String> {
    let converted = if cfg!(windows) {
        // /c/foo → C:\foo
        let bytes = path.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[2] == b'/'
            && bytes[1].is_ascii_alphabetic()
        {
            let drive = (bytes[1] as char).to_ascii_uppercase();
            format!("{}:{}", drive, path[2..].replace('/', "\\"))
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    };
    let canon = fbuild_core::path::canonicalize_existing(&converted)
        .await
        .map_err(|e| {
            fbuild_core::FbuildError::Other(format!("cannot resolve path '{}': {}", path, e))
        })?;
    Ok(canon.as_path().to_string_lossy().to_string())
}

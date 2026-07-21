//! Standalone project/framework cleanup command.

use crate::daemon_client::{self, BuildRequest, DaemonClient};
use crate::output;

use super::args::CleanScope;
use super::deploy::print_operation_streams;

pub async fn run_clean(
    project_dir: String,
    environment: Option<String>,
    scope: CleanScope,
    quick: bool,
    release: bool,
) -> fbuild_core::Result<()> {
    let profile = if release {
        Some("release".to_string())
    } else if quick {
        Some("quick".to_string())
    } else {
        None
    };
    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let client = DaemonClient::new();
    daemon_client::warn_if_daemon_identity_mismatch(&client, &project_dir).await;
    let req = BuildRequest {
        project_dir,
        environment,
        clean_build: true,
        clean_all: matches!(scope, CleanScope::All),
        clean_only: true,
        verbose: false,
        jobs: None,
        profile,
        generate_compiledb: false,
        compiledb_only: false,
        request_id: None,
        caller_pid,
        caller_cwd,
        stream: true,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: std::env::var("PLATFORMIO_SRC_DIR")
            .ok()
            .filter(|value| !value.is_empty()),
        output_dir: None,
        pio_env: daemon_client::capture_pio_env(),
        bloat_analysis: false,
    };

    let response = client.build_streaming(&req).await?;
    print_operation_streams(&response);
    if !response.success {
        output::error(response.message);
        std::process::exit(response.exit_code);
    }
    Ok(())
}

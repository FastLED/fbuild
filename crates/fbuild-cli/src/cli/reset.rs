//! `fbuild reset` — reboot a device without re-flashing.
//!
//! The port is opened by the daemon (`POST /api/reset`), never by this
//! process, so the device-lease admission added in #1864 covers this entry
//! point too: a board held by another workflow's exclusive lease is refused
//! before any serial port is touched (FastLED/fbuild#2151). This module keeps
//! only the CLI shell — project/environment resolution, the shared serial
//! selector, and output formatting.

use crate::daemon_client::{DaemonClient, ResetRequest, ensure_daemon_running};
use crate::output;

pub async fn run_reset(
    project_dir: String,
    environment: Option<String>,
    port: Option<String>,
    lease_id: Option<String>,
    verbose: bool,
) -> fbuild_core::Result<()> {
    // The shared selector resolves `ser=<USB serial>` to a node path at use
    // time; the resolved node is what the daemon admits and opens.
    let port = fbuild_serial::port_selector::resolve_port_arg(port)?;
    let project_path = std::path::Path::new(&project_dir);
    let ini_path = project_path.join("platformio.ini");

    // Read config to detect board/platform; the daemon maps the board id to
    // the same platform-specific reset sequence it always used.
    let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
    let env_name = if let Some(ref e) = environment {
        e.clone()
    } else {
        config
            .get_default_environment()
            .ok_or_else(|| {
                fbuild_core::FbuildError::ConfigError(
                    "no environment found in platformio.ini".to_string(),
                )
            })?
            .to_string()
    };

    let env_config = config.get_env_config(&env_name)?;
    let board = env_config.get("board").ok_or_else(|| {
        fbuild_core::FbuildError::ConfigError(format!(
            "no 'board' key in environment '{}'",
            env_name
        ))
    })?;

    // Determine port
    let port = port.ok_or_else(|| {
        fbuild_core::FbuildError::SerialError("no serial port specified (use --port)".to_string())
    })?;

    output::progress(format!("resetting device on {}...", port));
    ensure_daemon_running().await?;
    let client = DaemonClient::new();
    reset_via(&client, port, board, lease_id, verbose).await
}

/// Send the reset to the daemon and report it. Split out so the wire contract
/// (port, board and the caller's lease id) is unit-testable against a stub
/// daemon without spawning one.
async fn reset_via(
    client: &DaemonClient,
    port: String,
    board: &str,
    lease_id: Option<String>,
    verbose: bool,
) -> fbuild_core::Result<()> {
    let response = client
        .device_reset(&ResetRequest {
            port,
            board: Some(board.to_string()),
            lease_id,
            verbose,
            request_id: None,
        })
        .await?;

    if !response.success {
        output::error(&response.message);
        return Err(fbuild_core::FbuildError::DaemonError(response.message));
    }
    output::result("device reset successful");
    Ok(())
}

#[cfg(test)]
#[path = "reset_tests.rs"]
mod tests;

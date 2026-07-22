//! `fbuild deploy`, `fbuild monitor`, and `fbuild test-emu` handlers,
//! plus the destination/emulator resolution helpers they share.

use super::build::open_in_browser;
use crate::daemon_client::{self, DaemonClient, DeployRequest, OperationResponse, TestEmuRequest};
use crate::output;
use fbuild_serial::{SerialClientMessage, SerialServerMessage};
use futures::{SinkExt, StreamExt};
use std::io::Write;
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliEmulatorKind {
    Qemu,
    Avr8js,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliDeployRoute {
    Device,
    Emulator(CliEmulatorKind),
}

pub fn infer_cli_default_emulator_kind(
    project_dir: &str,
    environment: Option<&str>,
) -> fbuild_core::Result<Option<CliEmulatorKind>> {
    let project_dir = std::path::Path::new(project_dir);
    let config = fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini"))
        .map_err(|e| {
            fbuild_core::FbuildError::Other(format!("failed to parse platformio.ini: {}", e))
        })?;
    let env_name = environment
        .map(|s| s.to_string())
        .or_else(|| config.get_default_environment().map(|s| s.to_string()))
        .unwrap_or_else(|| "default".to_string());
    let env_config = config.get_env_config(&env_name).map_err(|e| {
        fbuild_core::FbuildError::Other(format!("invalid environment '{}': {}", env_name, e))
    })?;
    let platform_str = env_config.get("platform").cloned().unwrap_or_default();
    let Some(platform) = fbuild_core::Platform::from_platform_str(&platform_str) else {
        return Ok(None);
    };
    let Some(board_id) = env_config.get("board").cloned() else {
        return Ok(None);
    };
    let board_overrides = config.get_board_overrides(&env_name).unwrap_or_default();
    let board = fbuild_config::BoardConfig::from_board_id_with_override_fallback(
        &board_id,
        &board_overrides,
        Some(project_dir),
    );
    Ok(
        match (platform, board.as_ref().map(|board| board.mcu.as_str())) {
            (fbuild_core::Platform::AtmelAvr, _) | (fbuild_core::Platform::AtmelMegaAvr, _) => {
                Some(CliEmulatorKind::Avr8js)
            }
            (fbuild_core::Platform::Espressif32, Some(mcu))
                if mcu.eq_ignore_ascii_case("esp32s3") =>
            {
                Some(CliEmulatorKind::Qemu)
            }
            _ => None,
        },
    )
}

pub fn resolve_cli_deploy_route(
    to: Option<&str>,
    emulator: Option<&str>,
    target: Option<&str>,
    qemu: bool,
    default_emulator: Option<CliEmulatorKind>,
) -> fbuild_core::Result<CliDeployRoute> {
    if let Some(target) = target {
        return match target {
            "device" => Ok(CliDeployRoute::Device),
            "qemu" => Ok(CliDeployRoute::Emulator(CliEmulatorKind::Qemu)),
            "avr8js" => Ok(CliDeployRoute::Emulator(CliEmulatorKind::Avr8js)),
            other => Err(fbuild_core::FbuildError::Other(format!(
                "unsupported deploy target '{}'",
                other
            ))),
        };
    }

    match to.unwrap_or("device") {
        "device" => {
            if qemu {
                return Err(fbuild_core::FbuildError::Other(
                    "--qemu cannot be combined with --to device".to_string(),
                ));
            }
            if let Some(emulator) = emulator {
                return Err(fbuild_core::FbuildError::Other(format!(
                    "--emulator {} requires --to emu",
                    emulator
                )));
            }
            Ok(CliDeployRoute::Device)
        }
        "emu" | "emulator" => {
            let emulator = if qemu {
                if let Some(explicit) = emulator {
                    if explicit != "qemu" {
                        return Err(fbuild_core::FbuildError::Other(
                            "--qemu cannot be combined with a different --emulator".to_string(),
                        ));
                    }
                }
                "qemu"
            } else {
                match emulator {
                    Some(explicit) => explicit,
                    None => match default_emulator {
                        Some(CliEmulatorKind::Qemu) => "qemu",
                        Some(CliEmulatorKind::Avr8js) => "avr8js",
                        None => {
                            return Err(fbuild_core::FbuildError::Other(
                                "--to emu requires an explicit --emulator for this board"
                                    .to_string(),
                            ));
                        }
                    },
                }
            };
            match emulator {
                "qemu" => Ok(CliDeployRoute::Emulator(CliEmulatorKind::Qemu)),
                "avr8js" => Ok(CliDeployRoute::Emulator(CliEmulatorKind::Avr8js)),
                other => Err(fbuild_core::FbuildError::Other(format!(
                    "unsupported emulator '{}'",
                    other
                ))),
            }
        }
        other => Err(fbuild_core::FbuildError::Other(format!(
            "unsupported deploy destination '{}'",
            other
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_deploy(
    project_dir: String,
    environment: Option<String>,
    port: Option<String>,
    protocol: Option<String>,
    clean: bool,
    clean_all: bool,
    monitor_after: bool,
    verbose: bool,
    timeout: Option<f64>,
    halt_on_error: Option<String>,
    halt_on_success: Option<String>,
    expect: Option<String>,
    no_timestamp: bool,
    skip_build: bool,
    qemu: bool,
    qemu_timeout: u32,
    baud_rate: Option<u32>,
    no_probe_rs: bool,
    to: Option<String>,
    emulator: Option<String>,
    target: Option<String>,
    output_dir: Option<String>,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();
    daemon_client::warn_if_daemon_identity_mismatch(&client, &project_dir).await;

    let default_emulator = if matches!(to.as_deref(), Some("emu" | "emulator"))
        && emulator.is_none()
        && target.is_none()
        && !qemu
    {
        infer_cli_default_emulator_kind(&project_dir, environment.as_deref())?
    } else {
        None
    };
    let deploy_route = resolve_cli_deploy_route(
        to.as_deref(),
        emulator.as_deref(),
        target.as_deref(),
        qemu,
        default_emulator,
    )?;

    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let req = DeployRequest {
        project_dir,
        environment,
        port,
        protocol,
        monitor_after,
        skip_build,
        clean_build: clean || clean_all,
        clean_all,
        verbose,
        monitor_timeout: timeout,
        monitor_halt_on_error: halt_on_error,
        monitor_halt_on_success: halt_on_success,
        monitor_expect: expect,
        monitor_show_timestamp: !no_timestamp,
        baud_rate,
        no_probe_rs,
        to,
        emulator,
        target,
        qemu,
        qemu_timeout,
        request_id: None,
        caller_pid,
        caller_cwd,
        src_dir: std::env::var("PLATFORMIO_SRC_DIR")
            .ok()
            .filter(|s| !s.is_empty()),
        output_dir,
        pio_env: daemon_client::capture_pio_env(),
    };

    let resp = client.deploy(&req).await?;
    // Physical deployers also return transport diagnostics in stdout/stderr
    // (for example the RP2040 mass-storage and managed-picotool errors). Keep
    // those visible instead of replaying streams only for emulator routes.
    let message_is_streamed = operation_streams_include_message(&resp);
    print_operation_streams(&resp);
    if !message_is_streamed {
        output::result(&resp.message);
    }
    if !resp.success {
        // process::exit skips normal destructor-based stdio flushing. Preserve
        // the daemon's final stdout/stderr when fbuild is piped by automation.
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        std::process::exit(resp.exit_code);
    }
    // Open browser for avr8js only when daemon returned a launch URL (non-headless mode)
    if deploy_route == CliDeployRoute::Emulator(CliEmulatorKind::Avr8js) {
        if let Some(url) = resp.launch_url.as_deref() {
            if let Err(e) = open_in_browser(url).await {
                output::warn(format!("failed to open browser: {}", e));
                output::warn(format!("open this URL manually: {}", url));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn run_test_emu(
    project_dir: String,
    environment: Option<String>,
    verbose: bool,
    timeout: Option<f64>,
    halt_on_error: Option<String>,
    halt_on_success: Option<String>,
    expect: Option<String>,
    no_timestamp: bool,
    emulator: Option<String>,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();
    daemon_client::warn_if_daemon_identity_mismatch(&client, &project_dir).await;

    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let req = TestEmuRequest {
        project_dir,
        environment,
        verbose,
        timeout,
        halt_on_error,
        halt_on_success,
        expect,
        emulator,
        show_timestamp: !no_timestamp,
        request_id: None,
        caller_pid,
        caller_cwd,
        pio_env: daemon_client::capture_pio_env(),
    };

    let resp = client.test_emu(&req).await?;
    let message_is_streamed = operation_streams_include_message(&resp);
    print_operation_streams(&resp);
    if !message_is_streamed {
        output::result(&resp.message);
    }
    if !resp.success {
        // Guarantee a non-zero exit when the daemon reports failure. A
        // structured error response carries `exit_code`, but if the
        // daemon handler or an intermediate proxy returns 0 alongside
        // success=false (issue #130), we must still surface failure to
        // the shell rather than silently exiting 0.
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        let code = if resp.exit_code == 0 {
            1
        } else {
            resp.exit_code
        };
        std::process::exit(code);
    }
    Ok(())
}

pub fn print_operation_streams(resp: &OperationResponse) {
    if let Some(stdout) = resp
        .stdout
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        // result() always appends one '\n'; strip a trailing newline so we don't
        // double up. If there was no trailing newline, result() supplies the one
        // the caller would have added with println!().
        output::result(stdout.trim_end_matches('\n'));
    }
    if let Some(stderr) = resp
        .stderr
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        // This is final operation output returned by the daemon, not transient
        // progress. It must remain visible under the default tracing filter and
        // when the command exits non-zero.
        output::diagnostic(stderr.trim_end_matches('\n'));
    }
}

fn operation_streams_include_message(resp: &OperationResponse) -> bool {
    let message = resp.message.trim();
    !message.is_empty()
        && [resp.stdout.as_deref(), resp.stderr.as_deref()]
            .into_iter()
            .flatten()
            .any(|stream| stream.trim() == message)
}

#[allow(clippy::too_many_arguments)]
pub async fn run_monitor(
    project_dir: String,
    environment: Option<String>,
    port: Option<String>,
    baud_rate: Option<u32>,
    timeout: Option<f64>,
    halt_on_error: Option<String>,
    halt_on_success: Option<String>,
    expect: Option<String>,
    no_timestamp: bool,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();
    daemon_client::warn_if_daemon_identity_mismatch(&client, &project_dir).await;

    let _ = (project_dir, environment);
    let port = match port {
        Some(port) => port,
        None => {
            let ports = fbuild_serial::ports::available_ports().map_err(|e| {
                fbuild_core::FbuildError::SerialError(format!(
                    "failed to enumerate serial ports: {e}"
                ))
            })?;
            match ports.as_slice() {
                [port] => port.port_name.clone(),
                [] => {
                    return Err(fbuild_core::FbuildError::SerialError(
                        "no serial ports detected; specify one with --port".to_string(),
                    ));
                }
                ports => {
                    let names = ports
                        .iter()
                        .map(|p| p.port_name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(fbuild_core::FbuildError::SerialError(format!(
                        "multiple serial ports detected ({names}); specify one with --port"
                    )));
                }
            }
        }
    };
    let baud_rate = baud_rate.unwrap_or(115200);
    let client_id = format!("fbuild-monitor-{}", std::process::id());
    let (mut socket, _) = connect_async(client.websocket_url("/ws/serial-monitor"))
        .await
        .map_err(|e| {
            fbuild_core::FbuildError::DaemonError(format!("monitor connection failed: {e}"))
        })?;
    let attach = SerialClientMessage::Attach {
        client_id,
        port,
        baud_rate,
        open_if_needed: true,
        pre_acquire_writer: false,
        client_metadata: None,
    };
    socket
        .send(Message::Text(serde_json::to_string(&attach).map_err(
            |e| fbuild_core::FbuildError::DaemonError(format!("monitor attach failed: {e}")),
        )?))
        .await
        .map_err(|e| {
            fbuild_core::FbuildError::DaemonError(format!("monitor attach failed: {e}"))
        })?;

    let halt_error = halt_on_error.and_then(|p| regex::Regex::new(&p).ok());
    let halt_success = halt_on_success.and_then(|p| regex::Regex::new(&p).ok());
    let expect_re = expect.and_then(|p| regex::Regex::new(&p).ok());
    let started = Instant::now();
    let deadline = timeout.map(|secs| started + Duration::from_secs_f64(secs));
    let mut expect_found = false;
    loop {
        let next = async { socket.next().await };
        let message = tokio::select! {
            message = next => message,
            _ = tokio::signal::ctrl_c() => break,
            _ = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline.into()).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => break,
        };
        let Some(Ok(Message::Text(text))) = message else {
            break;
        };
        let event: SerialServerMessage = serde_json::from_str(&text).map_err(|e| {
            fbuild_core::FbuildError::DaemonError(format!("invalid monitor event: {e}"))
        })?;
        match event {
            SerialServerMessage::Attached {
                success: false,
                message,
                ..
            }
            | SerialServerMessage::Error { message } => {
                return Err(fbuild_core::FbuildError::SerialError(message));
            }
            SerialServerMessage::Attached { success: true, .. } => {}
            SerialServerMessage::Data { lines, .. } => {
                for line in lines {
                    if !no_timestamp {
                        let elapsed = started.elapsed().as_secs_f64();
                        println!(
                            "{:02}:{:05.2} {}",
                            (elapsed / 60.0) as u64,
                            elapsed % 60.0,
                            line
                        );
                    } else {
                        println!("{line}");
                    }
                    if expect_re.as_ref().is_some_and(|re| re.is_match(&line)) {
                        expect_found = true;
                    }
                    if halt_error.as_ref().is_some_and(|re| re.is_match(&line))
                        || halt_success.as_ref().is_some_and(|re| re.is_match(&line))
                    {
                        let _ = socket
                            .send(Message::Text(
                                serde_json::to_string(&SerialClientMessage::Detach).unwrap(),
                            ))
                            .await;
                        return Ok(());
                    }
                }
            }
            SerialServerMessage::PortDisconnected { message, .. }
            | SerialServerMessage::PortRebindFailed { message, .. } => {
                return Err(fbuild_core::FbuildError::SerialError(message));
            }
            _ => {}
        }
    }
    let _ = socket
        .send(Message::Text(
            serde_json::to_string(&SerialClientMessage::Detach).unwrap(),
        ))
        .await;
    if expect_re.is_some() && !expect_found {
        return Err(fbuild_core::FbuildError::SerialError(
            "monitor timeout reached before --expect matched".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(message: &str, stdout: Option<&str>, stderr: Option<&str>) -> OperationResponse {
        OperationResponse {
            success: false,
            request_id: "request-1".to_string(),
            message: message.to_string(),
            exit_code: 1,
            output_file: None,
            output_dir: None,
            launch_url: None,
            stdout: stdout.map(str::to_string),
            stderr: stderr.map(str::to_string),
        }
    }

    #[test]
    fn identical_streamed_error_suppresses_duplicate_result_message() {
        let resp = response(
            "deploy error: transport failed",
            None,
            Some("deploy error: transport failed\n"),
        );
        assert!(operation_streams_include_message(&resp));
    }

    #[test]
    fn distinct_result_message_is_not_suppressed() {
        let resp = response("deploy failed", None, Some("transport detail"));
        assert!(!operation_streams_include_message(&resp));
    }
}

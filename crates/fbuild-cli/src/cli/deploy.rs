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
    usb_recovery_policy: fbuild_core::usb::UsbRecoveryPolicy,
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
        usb_recovery_policy,
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
    // FastLED/fbuild#1152: the daemon may attach a typed exact-device USB
    // recovery request. Apply the --admin/--no-admin policy here; with
    // explicit --admin this launches the one-shot elevated helper at most
    // once and then retries/rescans on freshly enumerated transports only.
    let resp = maybe_recover_and_retry(resp, &client, &req, usb_recovery_policy).await?;
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

/// FastLED/fbuild#1152: consume a typed exact-device recovery request from
/// the daemon's deploy response.
///
/// Policy-gated: only an explicit interactive `--admin` launches the #1148
/// one-shot elevated helper, exactly once. After a successful helper run all
/// prior port/volume facts are discarded: a failed transfer is retried once
/// through a fresh deployment (never rebuilding), while an already-confirmed
/// flash only rescans for the recovered runtime endpoint. Default,
/// `--no-admin`, CI, and non-interactive sessions never elevate.
async fn maybe_recover_and_retry(
    resp: OperationResponse,
    client: &DaemonClient,
    req: &DeployRequest,
    policy: fbuild_core::usb::UsbRecoveryPolicy,
) -> fbuild_core::Result<OperationResponse> {
    use super::usb_recovery::{self, RecoveryRunOutcome};

    let Some(request) = resp.usb_recovery.clone() else {
        return Ok(resp);
    };
    output::warn(format!(
        "deploy target {} is a known-unhealthy Windows devnode{}",
        request.instance_id,
        request
            .problem_code
            .map(|code| format!(" (problem code {code})"))
            .unwrap_or_default()
    ));
    let context = usb_recovery::RecoveryLaunchContext {
        is_windows: cfg!(windows),
        is_ci: std::env::var_os("CI").is_some(),
        is_interactive: std::io::IsTerminal::is_terminal(&std::io::stdin()),
    };
    #[cfg(windows)]
    let outcome = {
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            usb_recovery::run_recovery_for_typed_request(
                policy,
                &request,
                context,
                &mut usb_recovery::WindowsUacLauncher,
            )
        })
        .await
        .map_err(|error| {
            fbuild_core::FbuildError::Other(format!("recovery helper task failed: {error}"))
        })??
    };
    #[cfg(not(windows))]
    let outcome = match usb_recovery::decide_recovery_launch(policy, true, context) {
        usb_recovery::RecoveryLaunchDecision::ManualGuidance => RecoveryRunOutcome::ManualGuidance,
        _ => RecoveryRunOutcome::RefuseNonInteractive,
    };
    match outcome {
        RecoveryRunOutcome::ManualGuidance => {
            if policy == fbuild_core::usb::UsbRecoveryPolicy::Default {
                output::warn(
                    "rerun with --admin to attempt a scoped one-shot Windows PnP recovery (UAC), or physically re-enter BOOTSEL (hold BOOT, tap RESET) and retry",
                );
            } else {
                output::warn("--no-admin: privileged recovery skipped by request");
            }
            Ok(resp)
        }
        RecoveryRunOutcome::RefuseNonInteractive => {
            output::warn("scoped PnP recovery needs an interactive Windows session; not elevating");
            Ok(resp)
        }
        RecoveryRunOutcome::Cancelled => {
            output::warn("UAC prompt was cancelled; no recovery was attempted");
            Ok(resp)
        }
        RecoveryRunOutcome::Completed(result) => {
            output::result(format!(
                "one-shot PnP recovery {}: {:?} on {} ({:?} -> {:?})",
                if result.success {
                    "succeeded"
                } else {
                    "failed"
                },
                result.operation,
                result
                    .validated_instance_id
                    .as_deref()
                    .unwrap_or("(unvalidated)"),
                result.before,
                result.after,
            ));
            if !result.success {
                output::warn(format!(
                    "recovery helper reported {}; physical BOOTSEL replug remains the fallback",
                    result
                        .error_code
                        .as_deref()
                        .unwrap_or("an unspecified failure")
                ));
                return Ok(resp);
            }
            if !request.flash_completed {
                // The transfer never succeeded: one fresh deployment attempt
                // through freshly enumerated transports, never rebuilding and
                // never re-entering recovery (the retry runs with DenyAdmin).
                output::result("retrying the deployment once on freshly enumerated transports");
                let mut retry_req = req.clone();
                retry_req.usb_recovery_policy = fbuild_core::usb::UsbRecoveryPolicy::DenyAdmin;
                retry_req.skip_build = true;
                retry_req.clean_build = false;
                retry_req.clean_all = false;
                let retry = client.deploy(&retry_req).await?;
                let retry_streamed = operation_streams_include_message(&retry);
                print_operation_streams(&retry);
                if !retry_streamed {
                    output::result(&retry.message);
                }
                return Ok(retry);
            }
            // Flash already confirmed: never reflash for recovery. Rescan for
            // the freshly enumerated, health-eligible, openable endpoint.
            match reacquire_recovered_port(&request).await {
                Some(port) => {
                    output::result(format!(
                        "recovered runtime CDC endpoint {port} after PnP recovery"
                    ));
                }
                None => {
                    output::warn(
                        "no healthy runtime CDC endpoint appeared after recovery; physical BOOTSEL replug remains the fallback",
                    );
                }
            }
            Ok(resp)
        }
    }
}

/// Bounded post-recovery rescan (FastLED/fbuild#1152): fresh enumerations
/// only, health-eligible records only, and a bounded open probe before any
/// name is reported. Stale COM names and prior scan facts are never reused.
async fn reacquire_recovered_port(
    request: &fbuild_core::usb::UsbRecoveryRequest,
) -> Option<String> {
    const REACQUIRE_WINDOW: Duration = Duration::from_secs(10);
    const REACQUIRE_POLL: Duration = Duration::from_millis(500);
    let expected_serial = request.expected_serial.clone();
    let expected_vid = request.expected_vid;
    let expected_pid = request.expected_pid;
    let started = Instant::now();
    while started.elapsed() < REACQUIRE_WINDOW {
        let expected_serial = expected_serial.clone();
        let found = tokio::task::spawn_blocking(move || {
            let ports = fbuild_serial::ports::available_ports().ok()?;
            ports
                .into_iter()
                .filter(|port| !port.health.is_known_unhealthy())
                .find_map(|port| {
                    let serialport::SerialPortType::UsbPort(usb) = &port.info.port_type else {
                        return None;
                    };
                    let identity_matches = match expected_serial.as_deref() {
                        Some(serial) => usb.serial_number.as_deref() == Some(serial),
                        None => (usb.vid, usb.pid) == (expected_vid, expected_pid),
                    };
                    if !identity_matches {
                        return None;
                    }
                    serialport::new(&port.info.port_name, 115_200)
                        .timeout(Duration::from_millis(250))
                        .open()
                        .ok()
                        .map(|_| port.info.port_name)
                })
        })
        .await
        .ok()
        .flatten();
        if found.is_some() {
            return found;
        }
        tokio::time::sleep(REACQUIRE_POLL).await;
    }
    None
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

fn select_monitor_port(
    requested: Option<String>,
    ports: &[fbuild_serial::ports::DetectedPort],
) -> fbuild_core::Result<String> {
    match requested {
        Some(port) => {
            if let Some(detected) = ports
                .iter()
                .find(|detected| detected.info.port_name == port)
                .filter(|detected| detected.health.is_known_unhealthy())
            {
                return Err(fbuild_core::FbuildError::SerialError(format!(
                    "refusing to monitor {port}: endpoint health is {}; run `fbuild port scan` for recovery details",
                    detected.health.label()
                )));
            }
            Ok(port)
        }
        None => {
            let selectable = ports
                .iter()
                .filter(|port| !port.health.is_known_unhealthy())
                .collect::<Vec<_>>();
            match selectable.as_slice() {
                [port] => Ok(port.info.port_name.clone()),
                [] if ports.is_empty() => Err(fbuild_core::FbuildError::SerialError(
                    "no serial ports detected; specify one with --port".to_string(),
                )),
                [] => {
                    let unhealthy = ports
                        .iter()
                        .map(|port| format!("{} ({})", port.info.port_name, port.health.label()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    Err(fbuild_core::FbuildError::SerialError(format!(
                        "no selectable serial ports detected ({unhealthy}); run `fbuild port scan` for recovery details"
                    )))
                }
                selectable => {
                    let names = selectable
                        .iter()
                        .map(|port| port.info.port_name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    Err(fbuild_core::FbuildError::SerialError(format!(
                        "multiple serial ports detected ({names}); specify one with --port"
                    )))
                }
            }
        }
    }
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
    let ports = fbuild_serial::ports::available_ports().map_err(|e| {
        fbuild_core::FbuildError::SerialError(format!("failed to enumerate serial ports: {e}"))
    })?;
    let port = select_monitor_port(port, &ports)?;
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

    fn detected_port(
        name: &str,
        health: fbuild_serial::ports::PortHealth,
    ) -> fbuild_serial::ports::DetectedPort {
        fbuild_serial::ports::DetectedPort {
            info: serialport::SerialPortInfo {
                port_name: name.to_string(),
                port_type: serialport::SerialPortType::Unknown,
            },
            health,
            instance_id: None,
            parent_instance_id: None,
        }
    }

    fn response(message: &str, stdout: Option<&str>, stderr: Option<&str>) -> OperationResponse {
        OperationResponse {
            usb_recovery: None,
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

    #[test]
    fn monitor_selection_never_chooses_a_known_unhealthy_endpoint() {
        let ports = vec![detected_port(
            "COM12",
            fbuild_serial::ports::PortHealth::Phantom {
                problem_code: Some(45),
                status: Some(0),
            },
        )];

        let auto_error = select_monitor_port(None, &ports).unwrap_err().to_string();
        assert!(auto_error.contains("no selectable serial ports"));
        let explicit_error = select_monitor_port(Some("COM12".to_string()), &ports)
            .unwrap_err()
            .to_string();
        assert!(explicit_error.contains("endpoint health is phantom"));
    }
}

//! `fbuild deploy`, `fbuild monitor`, and `fbuild test-emu` handlers,
//! plus the destination/emulator resolution helpers they share.

use super::build::open_in_browser;
use crate::daemon_client::{
    self, DaemonClient, DeployRequest, MonitorRequest, OperationResponse, TestEmuRequest,
};

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
    let board = fbuild_config::BoardConfig::from_board_id(&board_id, &board_overrides)
        .or_else(|_| {
            fbuild_config::BoardConfig::from_board_id(&board_id, &std::collections::HashMap::new())
        })
        .ok();
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
                            ))
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
    clean: bool,
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
    to: Option<String>,
    emulator: Option<String>,
    target: Option<String>,
    output_dir: Option<String>,
) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let client = DaemonClient::new();

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
        monitor_after,
        skip_build,
        clean_build: clean,
        verbose,
        monitor_timeout: timeout,
        monitor_halt_on_error: halt_on_error,
        monitor_halt_on_success: halt_on_success,
        monitor_expect: expect,
        monitor_show_timestamp: !no_timestamp,
        baud_rate,
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
    if deploy_route == CliDeployRoute::Emulator(CliEmulatorKind::Qemu)
        || deploy_route == CliDeployRoute::Emulator(CliEmulatorKind::Avr8js)
    {
        print_operation_streams(&resp);
    }
    println!("{}", resp.message);
    if !resp.success {
        std::process::exit(resp.exit_code);
    }
    // Open browser for avr8js only when daemon returned a launch URL (non-headless mode)
    if deploy_route == CliDeployRoute::Emulator(CliEmulatorKind::Avr8js) {
        if let Some(url) = resp.launch_url.as_deref() {
            if let Err(e) = open_in_browser(url) {
                eprintln!("warning: failed to open browser: {}", e);
                eprintln!("open this URL manually: {}", url);
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
    print_operation_streams(&resp);
    println!("{}", resp.message);
    if !resp.success {
        // Guarantee a non-zero exit when the daemon reports failure. A
        // structured error response carries `exit_code`, but if the
        // daemon handler or an intermediate proxy returns 0 alongside
        // success=false (issue #130), we must still surface failure to
        // the shell rather than silently exiting 0.
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
        print!("{}", stdout);
        if !stdout.ends_with('\n') {
            println!();
        }
    }
    if let Some(stderr) = resp
        .stderr
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        eprint!("{}", stderr);
        if !stderr.ends_with('\n') {
            eprintln!();
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

    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let req = MonitorRequest {
        project_dir,
        environment,
        port,
        baud_rate,
        halt_on_error,
        halt_on_success,
        expect,
        timeout,
        show_timestamp: !no_timestamp,
        request_id: None,
        caller_pid,
        caller_cwd,
    };

    let resp = client.monitor(&req).await?;
    println!("{}", resp.message);
    if !resp.success {
        std::process::exit(resp.exit_code);
    }
    Ok(())
}

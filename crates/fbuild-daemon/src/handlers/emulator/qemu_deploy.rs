//! `POST /api/deploy` (QEMU variant) — builds the flash image, resolves the
//! Espressif QEMU binary, and streams the run through the shared process
//! runner. Also exposes helpers used by the runner-trait implementations
//! (`resolve_esp_qemu_for_mcu`, `check_qemu_flash_mode`,
//! `is_qemu_supported_esp32_mcu`).

use super::shared::{
    build_linux_macos_qemu_hint, qemu_session_dir, resolve_esp32_toolchain_gcc_path,
    run_qemu_process, RunQemuOptions,
};
use crate::context::DaemonContext;
use crate::handlers::operations::MonitorOutcome;
use crate::models::OperationResponse;
use axum::http::StatusCode;
use axum::Json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct DeployQemuRequest {
    pub request_id: String,
    pub project_dir: PathBuf,
    pub env_name: String,
    pub board_id: String,
    pub platform: fbuild_core::Platform,
    pub firmware_path: PathBuf,
    pub elf_path: Option<PathBuf>,
    pub output_file: String,
    pub output_dir: Option<String>,
    pub monitor_timeout: Option<f64>,
    pub qemu_timeout_secs: u32,
    pub halt_on_error: Option<String>,
    pub halt_on_success: Option<String>,
    pub expect: Option<String>,
    pub show_timestamp: bool,
    pub verbose: bool,
    pub board_overrides: HashMap<String, String>,
}

/// Check whether a given MCU is supported by the QEMU runner.
///
/// Supported MCUs:
/// - Xtensa (`qemu-system-xtensa`): `esp32`, `esp32s3`
/// - RISC-V (`qemu-system-riscv32`): `esp32c3`, `esp32c6`, `esp32h2`
pub(crate) fn is_qemu_supported_esp32_mcu(mcu: &str) -> bool {
    fbuild_packages::toolchain::EspQemuArch::for_mcu(mcu).is_some()
}

/// Resolve the Espressif QEMU executable appropriate for the given MCU.
///
/// Picks `qemu-system-xtensa` for ESP32/ESP32-S3 and `qemu-system-riscv32`
/// for ESP32-C3/C6/H2. Returns the resolved binary path (downloading into
/// the managed fbuild cache if required).
pub(crate) async fn resolve_esp_qemu_for_mcu(
    project_dir: &Path,
    mcu: &str,
) -> fbuild_core::Result<PathBuf> {
    let arch = fbuild_packages::toolchain::EspQemuArch::for_mcu(mcu).ok_or_else(|| {
        fbuild_core::FbuildError::DeployFailed(format!(
            "no QEMU backend available for MCU '{}'",
            mcu
        ))
    })?;
    let pkg = fbuild_packages::toolchain::EspQemu::new(project_dir, arch)?;
    pkg.resolve_executable().await
}

/// Fail fast if the board's flash mode is incompatible with QEMU (DIO only).
pub(crate) fn check_qemu_flash_mode(board: &fbuild_config::BoardConfig) -> fbuild_core::Result<()> {
    let mcu_config = fbuild_build::esp32::mcu_config::get_mcu_config(&board.mcu)?;
    let effective_flash_mode = board
        .flash_mode
        .as_deref()
        .unwrap_or(mcu_config.default_flash_mode());
    if !effective_flash_mode.eq_ignore_ascii_case("dio") {
        return Err(fbuild_core::FbuildError::DeployFailed(format!(
            "QEMU requires DIO flash mode; board '{}' uses '{}'",
            board.name, effective_flash_mode
        )));
    }
    Ok(())
}

pub async fn deploy_qemu(
    _ctx: Arc<DaemonContext>,
    req: DeployQemuRequest,
) -> (StatusCode, Json<OperationResponse>) {
    let DeployQemuRequest {
        request_id,
        project_dir,
        env_name,
        board_id,
        platform,
        firmware_path,
        elf_path,
        output_file,
        output_dir,
        monitor_timeout,
        qemu_timeout_secs,
        halt_on_error,
        halt_on_success,
        expect,
        show_timestamp,
        verbose,
        board_overrides,
    } = req;

    if platform != fbuild_core::Platform::Espressif32 {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                "QEMU deploy target is currently supported only for ESP32-family boards"
                    .to_string(),
            )),
        );
    }
    if firmware_path.extension().and_then(|ext| ext.to_str()) != Some("bin") {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!(
                    "QEMU deploy target requires firmware.bin, got '{}'",
                    firmware_path.display()
                ),
            )),
        );
    }

    let board = match fbuild_config::BoardConfig::from_board_id_in_project(
        &board_id,
        &board_overrides,
        Some(project_dir.as_path()),
    ) {
        Ok(board) => board,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(
                    request_id,
                    format!("failed to load board config for QEMU: {}", e),
                )),
            );
        }
    };
    if !is_qemu_supported_esp32_mcu(&board.mcu) {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!(
                    "native QEMU deploy currently supports ESP32, ESP32-S3 (Xtensa) and \
                     ESP32-C3, ESP32-C6, ESP32-H2 (RISC-V) boards, got '{}'",
                    board.mcu
                ),
            )),
        );
    }

    let mcu_config = match fbuild_build::esp32::mcu_config::get_mcu_config(&board.mcu) {
        Ok(cfg) => cfg,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("failed to load MCU config for QEMU: {}", e),
                )),
            );
        }
    };

    let effective_flash_mode = board
        .flash_mode
        .as_deref()
        .unwrap_or(mcu_config.default_flash_mode());
    if !effective_flash_mode.eq_ignore_ascii_case("dio") {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!(
                    "QEMU requires a DIO-compatible flash image; effective flash mode is '{}'",
                    effective_flash_mode
                ),
            )),
        );
    }

    let flash_size_bytes = match fbuild_deploy::esp32::resolve_qemu_flash_size_bytes(
        &board,
        mcu_config.default_flash_size(),
    ) {
        Ok(size) => size,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(request_id, e.to_string())),
            );
        }
    };

    let qemu = match resolve_esp_qemu_for_mcu(&project_dir, &board.mcu).await {
        Ok(path) => path,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(
                    request_id,
                    build_linux_macos_qemu_hint(&e.to_string()),
                )),
            );
        }
    };

    let session_dir = qemu_session_dir(&project_dir, &env_name);
    if let Err(e) = std::fs::create_dir_all(&session_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("failed to create QEMU session dir: {}", e),
            )),
        );
    }
    let flash_image = session_dir.join("qemu_flash.bin");
    // Only apply the ESP32-S3 ADC calibration patch for S3 variants.
    let elf_for_adc_patch = if board.mcu.eq_ignore_ascii_case("esp32s3") {
        elf_path.as_deref()
    } else {
        None
    };
    if let Err(e) = fbuild_deploy::esp32::create_qemu_flash_image(
        &firmware_path,
        &flash_image,
        flash_size_bytes,
        mcu_config.bootloader_offset(),
        mcu_config.partitions_offset(),
        mcu_config.firmware_offset(),
        elf_for_adc_patch,
    ) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("failed to create QEMU flash image: {}", e),
            )),
        );
    }

    let args = fbuild_deploy::esp32::build_qemu_args(
        &board.mcu,
        &flash_image,
        board.qemu_esp32_psram_config(),
    );
    let addr2line_path = if elf_path.is_some() {
        match resolve_esp32_toolchain_gcc_path(&project_dir, &mcu_config).await {
            Ok(gcc) => fbuild_serial::crash_decoder::derive_addr2line_path(&gcc),
            Err(_) => None,
        }
    } else {
        None
    };

    let timeout_secs = monitor_timeout.or(Some(qemu_timeout_secs as f64));
    let qemu_result = match run_qemu_process(
        &qemu,
        &args,
        RunQemuOptions {
            elf_path,
            addr2line_path,
            timeout_secs,
            halt_on_error: halt_on_error.as_deref(),
            halt_on_success: halt_on_success.as_deref(),
            expect: expect.as_deref(),
            show_timestamp,
            verbose,
            process_label: "QEMU",
        },
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(request_id, e.to_string())),
            );
        }
    };

    let real_exit_code = qemu_result.exit_code;
    match qemu_result.outcome {
        MonitorOutcome::Success(message) => (
            StatusCode::OK,
            Json(OperationResponse {
                success: true,
                request_id,
                message: format!("QEMU run succeeded: {}", message),
                exit_code: real_exit_code.unwrap_or(0),
                output_file: Some(output_file),
                output_dir,
                launch_url: None,
                stdout: Some(qemu_result.stdout),
                stderr: Some(qemu_result.stderr),
            }),
        ),
        MonitorOutcome::Error(message) => (
            StatusCode::OK,
            Json(OperationResponse {
                success: false,
                request_id,
                message: format!("QEMU run failed: {}", message),
                exit_code: real_exit_code.unwrap_or(1),
                output_file: Some(output_file),
                output_dir,
                launch_url: None,
                stdout: Some(qemu_result.stdout),
                stderr: Some(qemu_result.stderr),
            }),
        ),
        MonitorOutcome::Timeout { expect_found } => {
            let success = expect.is_none() || expect_found;
            let exit_code = real_exit_code.unwrap_or(if success { 0 } else { 1 });
            (
                StatusCode::OK,
                Json(OperationResponse {
                    success,
                    request_id,
                    message: if success {
                        "QEMU run completed (timeout)".to_string()
                    } else {
                        "QEMU run timed out (expected pattern not found)".to_string()
                    },
                    exit_code,
                    output_file: Some(output_file),
                    output_dir,
                    launch_url: None,
                    stdout: Some(qemu_result.stdout),
                    stderr: Some(qemu_result.stderr),
                }),
            )
        }
        // QEMU does not drive real DTR/RTS lines; ESP auto-recovery is
        // unreachable from this path. Defensive arm.
        MonitorOutcome::RecoverDownloadMode { signal } => (
            StatusCode::OK,
            Json(OperationResponse {
                success: false,
                request_id,
                message: format!(
                    "internal: QEMU emitted ESP RecoverDownloadMode ({})",
                    signal.diagnostic()
                ),
                exit_code: real_exit_code.unwrap_or(1),
                output_file: Some(output_file),
                output_dir,
                launch_url: None,
                stdout: Some(qemu_result.stdout),
                stderr: Some(qemu_result.stderr),
            }),
        ),
    }
}

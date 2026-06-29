//! Runner selection (`select_runner`) and the build-then-emulate flow exposed
//! as `POST /api/test-emu` (`test_emu`).

use super::qemu_deploy::{check_qemu_flash_mode, is_qemu_supported_esp32_mcu};
use super::runners::{Avr8jsRunner, EmulatorRunner, QemuRunner, SimavrRunner};
use super::shared::EmulatorRunConfig;
use crate::context::DaemonContext;
use crate::models::OperationResponse;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use fbuild_core::emulator::EmulatorArtifactBundle;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Select the appropriate emulator runner based on platform, MCU, and optional
/// explicit emulator choice.
///
/// Returns `Err` with `EmulatorOutcome::Unsupported` information if no runner
/// matches.
pub fn select_runner(
    project_dir: &Path,
    env_name: &str,
    platform: fbuild_core::Platform,
    board_id: &str,
    board_overrides: &HashMap<String, String>,
    emulator: Option<&str>,
) -> fbuild_core::Result<Box<dyn EmulatorRunner>> {
    let board = fbuild_config::BoardConfig::from_board_id_in_project(
        board_id,
        board_overrides,
        Some(project_dir),
    )?;

    if let Some(explicit) = emulator {
        return match explicit {
            "qemu" => {
                if platform != fbuild_core::Platform::Espressif32 {
                    return Err(fbuild_core::FbuildError::DeployFailed(
                        "QEMU runner is only supported for ESP32-family boards".to_string(),
                    ));
                }
                if !is_qemu_supported_esp32_mcu(&board.mcu) {
                    return Err(fbuild_core::FbuildError::DeployFailed(format!(
                        "QEMU runner currently supports ESP32, ESP32-S3 (Xtensa) and \
                         ESP32-C3, ESP32-C6, ESP32-H2 (RISC-V), got '{}'",
                        board.mcu
                    )));
                }
                check_qemu_flash_mode(&board)?;
                Ok(Box::new(QemuRunner::new(
                    project_dir.to_path_buf(),
                    env_name.to_string(),
                    board,
                )))
            }
            "avr8js" => {
                if !matches!(
                    platform,
                    fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr
                ) {
                    return Err(fbuild_core::FbuildError::DeployFailed(
                        "avr8js runner is only supported for AVR boards".to_string(),
                    ));
                }
                if !board.mcu.eq_ignore_ascii_case("atmega328p") {
                    return Err(fbuild_core::FbuildError::DeployFailed(format!(
                        "avr8js runner currently supports only ATmega328P, got '{}'",
                        board.mcu
                    )));
                }
                Ok(Box::new(Avr8jsRunner::new(board)))
            }
            "simavr" => {
                if !matches!(
                    platform,
                    fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr
                ) {
                    return Err(fbuild_core::FbuildError::DeployFailed(
                        "simavr runner is only supported for AVR boards".to_string(),
                    ));
                }
                if !board.has_emulator("simavr") {
                    return Err(fbuild_core::FbuildError::DeployFailed(format!(
                        "board '{}' does not advertise simavr support in its debug_tools",
                        board_id
                    )));
                }
                Ok(Box::new(SimavrRunner::new(board)))
            }
            other => Err(fbuild_core::FbuildError::DeployFailed(format!(
                "unsupported emulator '{}'; available: qemu, avr8js, simavr",
                other
            ))),
        };
    }

    // Auto-detect based on platform and MCU
    match platform {
        fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr => {
            if board.mcu.eq_ignore_ascii_case("atmega328p") {
                // Default to avr8js for ATmega328P (no external binary needed)
                Ok(Box::new(Avr8jsRunner::new(board)))
            } else if board.has_emulator("simavr") {
                // Use simavr for other AVR MCUs that advertise it
                Ok(Box::new(SimavrRunner::new(board)))
            } else {
                Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "no emulator runner available for AVR MCU '{}'; \
                     ATmega328P is supported via avr8js, other AVR boards require simavr in debug_tools",
                    board.mcu
                )))
            }
        }
        fbuild_core::Platform::Espressif32 => {
            if is_qemu_supported_esp32_mcu(&board.mcu) {
                check_qemu_flash_mode(&board)?;
                Ok(Box::new(QemuRunner::new(
                    project_dir.to_path_buf(),
                    env_name.to_string(),
                    board,
                )))
            } else {
                Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "no emulator runner available for ESP32 MCU '{}'; \
                     ESP32, ESP32-S3 (Xtensa) and ESP32-C3, ESP32-C6, ESP32-H2 (RISC-V) \
                     are supported via QEMU",
                    board.mcu
                )))
            }
        }
        _ => Err(fbuild_core::FbuildError::DeployFailed(format!(
            "no emulator runner available for platform {:?}",
            platform
        ))),
    }
}

/// POST /api/test-emu handler — build firmware then run it in an emulator.
pub async fn test_emu(
    State(ctx): State<Arc<DaemonContext>>,
    Json(req): Json<crate::models::TestEmuRequest>,
) -> (StatusCode, Json<OperationResponse>) {
    let request_id = req
        .request_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let project_dir = PathBuf::from(&req.project_dir);

    // Mark the daemon as busy for the full build + emulate lifecycle.
    // Without this guard the 30 s self-eviction loop sees an "empty"
    // daemon during long (>30 s) ESP32/QEMU builds and triggers graceful
    // shutdown, which closes the in-flight HTTP connection and surfaces
    // as `error sending request for url (.../api/test-emu)` on the CLI
    // side. See issue #130.
    let _op_guard = crate::handlers::operations::OperationGuard::new(
        &ctx,
        fbuild_core::DaemonState::Building,
        Some(format!("test-emu {}", req.project_dir)),
    );

    if !project_dir.exists() {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!("project directory does not exist: {}", req.project_dir),
            )),
        );
    }

    // Parse config
    let config =
        match fbuild_config::PlatformIOConfig::from_path(&project_dir.join("platformio.ini")) {
            Ok(c) => c,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("failed to parse platformio.ini: {}", e),
                    )),
                );
            }
        };

    let env_name = req
        .environment
        .clone()
        .or_else(|| config.get_default_environment().map(|s| s.to_string()))
        .unwrap_or_else(|| "default".to_string());

    let env_config = match config.get_env_config(&env_name) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(
                    request_id,
                    format!("invalid environment '{}': {}", env_name, e),
                )),
            );
        }
    };

    let platform_str = env_config.get("platform").cloned().unwrap_or_default();
    let platform = match fbuild_core::Platform::from_platform_str(&platform_str) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(
                    request_id,
                    format!("unsupported platform: {}", platform_str),
                )),
            );
        }
    };

    let board_id = env_config.get("board").cloned().unwrap_or_else(|| {
        fbuild_build::get_platform_support(platform)
            .map(|s| s.default_board_id().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    });
    let board_overrides = config.get_board_overrides(&env_name).unwrap_or_default();

    // Select the emulator runner before building (fail fast on unsupported boards)
    let runner = match select_runner(
        &project_dir,
        &env_name,
        platform,
        &board_id,
        &board_overrides,
        req.emulator.as_deref(),
    ) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(request_id, e.to_string())),
            );
        }
    };

    // Build firmware (hold project lock only during build, not the emulator phase)
    let build_result = {
        let lock = ctx.project_lock(&project_dir);
        let _guard = lock.lock().await;

        let needs_qemu_flags = platform == fbuild_core::Platform::Espressif32
            && req.emulator.as_deref() != Some("avr8js");
        let board_for_flags = if needs_qemu_flags {
            fbuild_config::BoardConfig::from_board_id_in_project(
                &board_id,
                &board_overrides,
                Some(project_dir.as_path()),
            )
            .ok()
        } else {
            None
        };

        let build_dir = fbuild_paths::BuildLayout::new(
            project_dir.clone(),
            env_name.clone(),
            fbuild_core::BuildProfile::Release,
        )
        .with_override_root(req.build_dir_override.as_deref().map(|p| {
            let path = std::path::PathBuf::from(p);
            if path.is_absolute() {
                path
            } else if let Some(cwd) = req.caller_cwd.as_deref() {
                std::path::PathBuf::from(cwd).join(path)
            } else {
                project_dir.join(path)
            }
        }))
        .with_flatten_env(req.flatten_env)
        .resolve();
        let params = fbuild_build::BuildParams {
            project_dir: project_dir.clone(),
            env_name: env_name.clone(),
            clean: false,
            profile: fbuild_core::BuildProfile::Release,
            build_dir,
            verbose: req.verbose,
            jobs: None,
            generate_compiledb: false,
            compiledb_only: false,
            log_sender: None,
            symbol_analysis: false,
            symbol_analysis_path: None,
            no_timestamp: false,
            src_dir: None,
            pio_env: req.pio_env.clone(),
            extra_build_flags: if needs_qemu_flags {
                board_for_flags
                    .as_ref()
                    .map(|b| crate::handlers::operations::qemu_extra_build_flags(platform, &b.mcu))
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            watch_set_cache: Some(std::sync::Arc::clone(&ctx.watch_set_cache) as std::sync::Arc<_>),
            bloat_analysis: false,
        };

        let p = platform;
        match fbuild_build::get_orchestrator(p) {
            Ok(orchestrator) => orchestrator.build(&params).await,
            Err(e) => Err(e),
        }
    };

    let (firmware_path, elf_path) = match build_result {
        Ok(r) if r.success => {
            let fw = r.firmware_path.clone().unwrap_or_else(|| {
                r.elf_path
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("firmware.bin"))
            });
            (fw, r.elf_path)
        }
        Ok(r) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("build failed: {}", r.message),
                )),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("build error: {}", e),
                )),
            );
        }
    };

    // Run the emulator
    let artifact_bundle = EmulatorArtifactBundle::from_paths(&firmware_path, elf_path.as_deref());
    let run_config = EmulatorRunConfig {
        firmware_path,
        elf_path,
        artifact_bundle,
        timeout: req.timeout,
        halt_on_error: req.halt_on_error.clone(),
        halt_on_success: req.halt_on_success.clone(),
        expect: req.expect.clone(),
        show_timestamp: req.show_timestamp,
        verbose: req.verbose,
    };

    let emu_result = match runner.run(&run_config).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(OperationResponse::fail(
                    request_id,
                    format!("emulator error: {}", e),
                )),
            );
        }
    };

    let success = emu_result.is_success();
    let exit_code = emu_result.exit_code.unwrap_or(if success { 0 } else { 1 });
    let message = format!(
        "{} test-emu {}: {}",
        runner.name(),
        if success { "passed" } else { "failed" },
        emu_result.outcome
    );

    (
        StatusCode::OK,
        Json(OperationResponse {
            success,
            request_id,
            message,
            exit_code,
            output_file: None,
            output_dir: None,
            launch_url: None,
            stdout: Some(emu_result.stdout),
            stderr: Some(emu_result.stderr),
        }),
    )
}

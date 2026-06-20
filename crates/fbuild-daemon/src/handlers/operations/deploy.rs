//! `POST /api/deploy` — build (or reuse) firmware, flash, optionally monitor.

use super::common::{
    compute_esp32_image_hash, export_artifacts_bundle, infer_default_emulator_kind,
    parse_deploy_route, qemu_extra_build_flags, resolve_build_dir, resolve_client_path,
    trust_device_hash_enabled, DeployRoute, EmulatorKind, OperationGuard,
};
use super::deploy_port::{append_warning_to_stderr, choose_deploy_port};
use super::monitor::{run_monitor_loop, MonitorOutcome};
use crate::context::DaemonContext;
use crate::models::{DeployRequest, OperationResponse};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "espflash-native")]
use super::common::{native_verify_enabled, native_write_enabled};

/// Resolve a usable `teensy_loader_cli` binary for the Teensy deploy arm.
///
/// Search order:
///   1. `$PATH` (`teensy_loader_cli` on Unix, `teensy_loader_cli.exe` on Win)
///   2. `~/.platformio/packages/tool-teensy/teensy_loader_cli{.exe}` — the
///      well-known path PlatformIO installs it at on every PIO-using machine.
///
/// Returns `None` if neither is found; the TeensyDeployer's default will then
/// try a bare `teensy_loader_cli` invocation, which will surface
/// `command not found` to the user — clearer than a silent abort here.
fn find_teensy_loader_cli() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        "teensy_loader_cli.exe"
    } else {
        "teensy_loader_cli"
    };

    if let Ok(path_env) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for dir in path_env.split(sep) {
            let candidate = PathBuf::from(dir).join(exe_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // PlatformIO drops the binary here on every platform. Reusing it means a
    // user who already has PIO working doesn't need to install anything else
    // to deploy via fbuild.
    let pio_root = if cfg!(windows) {
        std::env::var("USERPROFILE").ok()
    } else {
        std::env::var("HOME").ok()
    };
    if let Some(home) = pio_root {
        let pio_candidate = PathBuf::from(home)
            .join(".platformio")
            .join("packages")
            .join("tool-teensy")
            .join(exe_name);
        if pio_candidate.is_file() {
            return Some(pio_candidate);
        }
    }

    None
}

/// POST /api/deploy
pub async fn deploy(
    State(ctx): State<Arc<DaemonContext>>,
    Json(req): Json<DeployRequest>,
) -> (StatusCode, Json<OperationResponse>) {
    let request_id = req
        .request_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let project_dir = PathBuf::from(&req.project_dir);

    if !project_dir.exists() {
        return (
            StatusCode::BAD_REQUEST,
            Json(OperationResponse::fail(
                request_id,
                format!("project directory does not exist: {}", req.project_dir),
            )),
        );
    }

    let _op_guard = OperationGuard::new(
        &ctx,
        fbuild_core::DaemonState::Deploying,
        Some(format!("Deploying {}", req.project_dir)),
    );

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
    let resolved_output_dir = req
        .output_dir
        .as_deref()
        .map(|p| resolve_client_path(p, req.caller_cwd.as_deref(), &project_dir));

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
    let board = fbuild_config::BoardConfig::from_board_id_with_override_fallback(
        &board_id,
        &board_overrides,
        Some(project_dir.as_path()),
    );
    let deploy_route = match parse_deploy_route(
        &req,
        board
            .as_ref()
            .and_then(|board| infer_default_emulator_kind(platform, &board.mcu)),
    ) {
        Ok(route) => route,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(OperationResponse::fail(request_id, e.to_string())),
            );
        }
    };

    // Build first unless skip_build
    let (firmware_path, elf_path) = if req.skip_build {
        // Look for existing firmware using the standard search order
        // (profiles: release/quick, base env dir, legacy .pio/build)
        match fbuild_paths::find_firmware(&project_dir, &env_name, None) {
            Some(path) => {
                let elf = path.parent().map(|dir| dir.join("firmware.elf"));
                let elf = elf.filter(|p| p.exists());
                (path, elf)
            }
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(OperationResponse::fail(
                        request_id,
                        "no firmware found; run build first or remove skip_build".to_string(),
                    )),
                );
            }
        }
    } else {
        // Run build first
        let lock = ctx.project_lock(&project_dir);
        let _guard = lock.lock().await;

        let build_dir = resolve_build_dir(
            req.build_dir_override.as_deref(),
            req.flatten_env,
            req.caller_cwd.as_deref(),
            &project_dir,
            &env_name,
            fbuild_core::BuildProfile::Release,
        );
        let params = fbuild_build::BuildParams {
            project_dir: project_dir.clone(),
            env_name: env_name.clone(),
            clean: req.clean_build,
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
            src_dir: req.src_dir,
            pio_env: req.pio_env,
            extra_build_flags: if deploy_route == DeployRoute::Emulator(EmulatorKind::Qemu) {
                board
                    .as_ref()
                    .map(|board| qemu_extra_build_flags(platform, &board.mcu))
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            watch_set_cache: Some(Arc::clone(&ctx.watch_set_cache) as Arc<_>),
            bloat_analysis: false,
        };

        let build_result = {
            let p = platform;
            tokio::task::spawn_blocking(move || {
                let orchestrator = fbuild_build::get_orchestrator(p)?;
                orchestrator.build(&params)
            })
            .await
        };

        match build_result {
            Ok(Ok(r)) if r.success => {
                let fw = r.firmware_path.clone().unwrap_or_else(|| {
                    r.elf_path
                        .clone()
                        .unwrap_or_else(|| PathBuf::from("firmware.bin"))
                });
                (fw, r.elf_path)
            }
            Ok(Ok(r)) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("build failed: {}", r.message),
                    )),
                );
            }
            Ok(Err(e)) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("build error: {}", e),
                    )),
                );
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("build task panicked: {}", e),
                    )),
                );
            }
        }
    };

    let artifact_export = match resolved_output_dir.as_ref() {
        Some(out_dir) => match export_artifacts_bundle(
            out_dir,
            platform,
            &env_name,
            Some(&firmware_path),
            elf_path.as_deref(),
        ) {
            Ok(result) => Some(result),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("failed to export artifacts: {}", e),
                    )),
                );
            }
        },
        None => None,
    };

    let reported_output_file = artifact_export
        .as_ref()
        .and_then(|r| r.primary_output.clone())
        .unwrap_or_else(|| firmware_path.clone())
        .to_string_lossy()
        .to_string();
    let reported_output_dir = artifact_export
        .as_ref()
        .map(|r| r.output_dir.to_string_lossy().to_string());

    if deploy_route == DeployRoute::Emulator(EmulatorKind::Avr8js) {
        return crate::handlers::emulator::deploy_avr8js(
            ctx,
            crate::handlers::emulator::DeployAvr8jsRequest {
                request_id,
                project_dir,
                env_name,
                board_id,
                platform,
                firmware_path,
                elf_path,
                monitor_after: req.monitor_after,
                output_file: reported_output_file,
                output_dir: reported_output_dir,
                monitor_timeout: req.monitor_timeout,
                halt_on_error: req.monitor_halt_on_error.clone(),
                halt_on_success: req.monitor_halt_on_success.clone(),
                expect: req.monitor_expect.clone(),
                show_timestamp: req.monitor_show_timestamp,
                verbose: req.verbose,
            },
        )
        .await;
    }

    if deploy_route == DeployRoute::Emulator(EmulatorKind::Qemu) {
        return crate::handlers::emulator::deploy_qemu(
            ctx,
            crate::handlers::emulator::DeployQemuRequest {
                request_id,
                project_dir,
                env_name,
                board_id,
                platform,
                firmware_path,
                elf_path,
                output_file: reported_output_file,
                output_dir: reported_output_dir,
                monitor_timeout: req.monitor_timeout,
                qemu_timeout_secs: req.qemu_timeout,
                halt_on_error: req.monitor_halt_on_error.clone(),
                halt_on_success: req.monitor_halt_on_success.clone(),
                expect: req.monitor_expect.clone(),
                show_timestamp: req.monitor_show_timestamp,
                verbose: req.verbose,
                board_overrides,
            },
        )
        .await;
    }

    let deploy_port_choice = if req.port.is_none() {
        ctx.refresh_devices_and_broadcast_serial_moves().await;
        choose_deploy_port(
            None,
            platform,
            board.as_ref(),
            ctx.device_manager.get_all_devices().into_values().collect(),
        )
    } else {
        choose_deploy_port(req.port.clone(), platform, board.as_ref(), Vec::new())
    };
    let deploy_port_warning = deploy_port_choice.warning.clone();

    // Preempt serial if port specified or auto-selected.
    let deploy_port_str = deploy_port_choice.port;
    if let Some(ref p) = deploy_port_str {
        let _ = ctx
            .serial_manager
            .preempt_for_deploy(p, "deploy".to_string(), request_id.clone())
            .await;
    }

    // Deploy
    let deploy_env = env_name.clone();
    let deploy_project = project_dir.clone();
    let deploy_port = deploy_port_str.clone();
    let deploy_fw = firmware_path.clone();
    let baud_override = req.baud_rate;
    let deploy_board_overrides = board_overrides.clone();
    // Snapshot the ctx pointer so the spawn_blocking closure can
    // consult / update the daemon's in-memory trusted-hash cache
    // without needing a cross-thread lock handshake.
    let ctx_for_deploy = Arc::clone(&ctx);
    let trusted_hash_enabled = trust_device_hash_enabled();
    // Refresh the enumeration cache so the trust-hash invalidation
    // path (`last_disconnect_at`) sees any unplug/replug that
    // happened between the previous deploy and now. Without this,
    // a user who swapped boards at the same COM port without hitting
    // a device-list endpoint could trip the trust check into a
    // false match.
    //
    // Back-to-back warm deploys (the 4 s / 1 s budget target) would
    // otherwise re-pay ~20–30 ms per deploy on Windows; cap the cost
    // at one enumeration per 2 s. The window is short enough that a
    // physically-sneaky board swap between two in-flight deploys
    // still needs to happen inside that window to trip trust, and
    // the trust-check still requires `is_connected == true` on the
    // cached DeviceState, which the most-recent refresh supplied.
    if trusted_hash_enabled {
        ctx.refresh_devices_if_stale_and_broadcast_serial_moves(std::time::Duration::from_secs(2))
            .await;
    }
    let deploy_result = tokio::task::spawn_blocking(move || -> fbuild_core::Result<(Option<Box<dyn fbuild_deploy::Deployer>>, fbuild_deploy::DeploymentResult)> {
        // Populated by the Espressif32 arm with (image_hash, port).
        // The tail of the closure consults it after `deployer.deploy`
        // returns to record or invalidate the daemon's trusted-hash
        // cache. Other platforms leave it `None`.
        let mut trusted_hash_update: Option<([u8; 32], String)> = None;
        let deployer: Box<dyn fbuild_deploy::Deployer> = match platform {
            fbuild_core::Platform::Espressif32 => {
                let board_config = fbuild_config::BoardConfig::from_board_id_or_default(
                    &board_id,
                    "esp32dev",
                    &deploy_board_overrides,
                    Some(deploy_project.as_path()),
                );
                // Load MCU config to get flash offsets and esptool defaults.
                // Fail loudly on an unknown MCU instead of silently falling
                // back to esp32's `0x1000` bootloader offset — that offset is
                // wrong for RISC-V variants (need `0x0`) and C5/P4 (need
                // `0x2000`), so the device would never boot (`invalid header`
                // reboot loop). The build path propagates this error too.
                let mcu_config = fbuild_build::esp32::mcu_config::get_mcu_config(&board_config.mcu)
                    .map_err(|e| {
                        fbuild_core::FbuildError::DeployFailed(format!(
                            "unsupported ESP32 MCU '{}' for board '{}': {} — cannot determine flash offsets",
                            board_config.mcu, board_id, e
                        ))
                    })?;
                // Flash mode: `board_config.flash_mode` is `None` for ESP32
                // chips unless the user explicitly set `board_build.flash_mode`
                // in their `[env:X]` section (see `BoardConfig::from_board_id`
                // — the JSON-shipped value is intentionally dropped for ESP32
                // because ESP32-S3's QIE-bit init is unreliable). The unwrap
                // therefore falls back to the per-MCU default "dio".
                let esptool_params = fbuild_deploy::esp32::EsptoolParams {
                    flash_mode: board_config
                        .flash_mode
                        .as_deref()
                        .unwrap_or(mcu_config.default_flash_mode())
                        .to_string(),
                    flash_freq: {
                        let f_for_image = board_config
                            .f_image
                            .as_deref()
                            .or(board_config.f_flash.as_deref());
                        fbuild_build::esp32::esp32_linker::f_flash_to_esptool_freq(
                            f_for_image,
                            mcu_config.default_flash_freq(),
                        )
                    },
                    flash_size: fbuild_build::esp32::mcu_config::bytes_to_flash_size(
                        board_config.max_flash,
                        mcu_config.default_flash_size(),
                    )
                    .to_string(),
                    default_baud: mcu_config.default_baud().to_string(),
                    before_reset: mcu_config.before_reset().to_string(),
                    after_reset: mcu_config.after_reset().to_string(),
                };
                let deployer = fbuild_deploy::esp32::Esp32Deployer::from_board_config(
                    &board_config,
                    mcu_config.bootloader_offset(),
                    mcu_config.partitions_offset(),
                    mcu_config.firmware_offset(),
                    &esptool_params,
                    false,
                );
                let deployer = if let Some(baud) = baud_override {
                    deployer.with_baud_rate(&baud.to_string())
                } else {
                    deployer
                };
                // Issue #66: native `verify-flash` + `write-flash` via
                // the `espflash` crate. This is compiled in by default;
                // `FBUILD_USE_ESPFLASH_*` are opt-out switches, and the
                // deployer falls back to esptool automatically when a
                // native operation fails.
                #[cfg(feature = "espflash-native")]
                let deployer = deployer
                    .with_native_verify(native_verify_enabled())
                    .with_native_write(native_write_enabled());

                // Compute a deterministic SHA-256 over the three
                // regions we'd otherwise verify-flash. Used twice
                // below: once to consult the daemon's trusted-hash
                // cache (opt-in via `FBUILD_TRUST_DEVICE_HASH=1`),
                // and once to *record* the hash after a successful
                // deploy so the next warm redeploy can skip the MD5
                // round-trip entirely. A `None` return means one of
                // the three files isn't on disk yet — treat it as
                // "can't trust-skip" rather than erroring, so the
                // fallback path is free to rebuild missing artefacts.
                let image_hash = compute_esp32_image_hash(
                    &ctx_for_deploy,
                    &deploy_fw,
                    u32::from_str_radix(
                        mcu_config.bootloader_offset().trim_start_matches("0x"),
                        16,
                    )
                    .unwrap_or(0),
                    u32::from_str_radix(
                        mcu_config.partitions_offset().trim_start_matches("0x"),
                        16,
                    )
                    .unwrap_or(0),
                    u32::from_str_radix(
                        mcu_config.firmware_offset().trim_start_matches("0x"),
                        16,
                    )
                    .unwrap_or(0),
                );

                // Session-trusted verify-skip: if the daemon last
                // flashed *this exact image* onto *this port* and the
                // port has been continuously enumerated since then,
                // no external agent could have re-flashed the chip
                // without breaking our enumeration (`last_disconnect_at`
                // is how we detect it). Skip the entire serial open +
                // espflash connect + MD5 round-trip.
                if let (Some(port), Some(hash)) = (deploy_port.as_deref(), image_hash) {
                    if trusted_hash_enabled {
                        if let Some(trusted) =
                            ctx_for_deploy.device_manager.trusted_firmware_hash(port)
                        {
                            if trusted == hash {
                                tracing::info!(
                                    port,
                                    "trusted-hash: session-trusted match; skipping verify-flash entirely"
                                );
                                // VerifySkip → recovery skipped (#605).
                                return Ok((None, fbuild_deploy::DeploymentResult {
                                    success: true,
                                    message: format!(
                                        "firmware already current on {} (skipped via session trust)",
                                        port
                                    ),
                                    port: Some(port.to_string()),
                                    stdout: String::new(),
                                    stderr: String::new(),
                                    outcome: fbuild_deploy::DeployOutcome::VerifySkip,
                                }));
                            }
                        }
                    }
                }

                // Fast deploy: ask the device whether it already holds
                // the exact firmware/bootloader/partitions we'd be about
                // to write. Uses esptool's `verify-flash` which dispatches
                // to the stub flasher's `FLASH_MD5SUM` command — no full
                // read-back, just one MD5 round-trip per region.
                //
                // Measured on a 2.4 MB FastLED esp32s3 image:
                //   * fresh write-flash: ~25 s
                //   * verify-flash skip: ~6 s   (-19 s, ~76% faster)
                //
                // Falls through to the normal flash path silently on
                // mismatch or transport error so we never break a deploy
                // that the verify call didn't understand.
                let mut selective_regions: Option<Vec<fbuild_deploy::esp32::FlashRegion>> = None;
                if let Some(port) = deploy_port.as_deref() {
                    match deployer.try_verify_deployment(&deploy_fw, port) {
                        Ok(fbuild_deploy::esp32::VerifyOutcome::Match { stdout, stderr }) => {
                            tracing::info!(
                                port,
                                "verify-flash: device already running this exact image; skipping write"
                            );
                            // VerifySkip → recovery skipped (#605).
                            return Ok((None, fbuild_deploy::DeploymentResult {
                                success: true,
                                message: format!(
                                    "firmware already current on {} (skipped via verify-flash)",
                                    port
                                ),
                                port: Some(port.to_string()),
                                stdout,
                                stderr,
                                outcome: fbuild_deploy::DeployOutcome::VerifySkip,
                            }));
                        }
                        Ok(fbuild_deploy::esp32::VerifyOutcome::Mismatch { regions, .. }) => {
                            // Pick only the regions that actually differ
                            // so we avoid the ~1s bootloader/partitions
                            // rewrite when only firmware changed. Empty
                            // `regions` means parsing failed — fall back
                            // to full flash.
                            let to_write: Vec<_> = regions
                                .iter()
                                .filter(|r| !r.matched)
                                .map(|r| r.region)
                                .collect();
                            if !regions.is_empty() && !to_write.is_empty() && to_write.len() < 3 {
                                tracing::info!(
                                    port,
                                    "verify-flash: only {} region(s) differ; flashing selectively",
                                    to_write.len()
                                );
                                selective_regions = Some(to_write);
                            } else {
                                tracing::info!(
                                    port,
                                    "verify-flash: device image differs; proceeding with full flash"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                port,
                                "verify-flash pre-check failed ({}); proceeding with full flash",
                                e
                            );
                        }
                    }
                }
                if let (Some(regions), Some(port)) = (selective_regions, deploy_port.as_deref()) {
                    let result = deployer.deploy_regions(&deploy_fw, port, &regions);
                    // Record/invalidate the trusted hash based on
                    // the selective-flash outcome so the next warm
                    // redeploy can short-circuit via trust-skip.
                    if let Some(hash) = image_hash {
                        match &result {
                            Ok(r) if r.success => {
                                ctx_for_deploy
                                    .device_manager
                                    .set_trusted_firmware_hash(port, hash);
                            }
                            _ => {
                                ctx_for_deploy.device_manager.clear_trusted_firmware_hash(port);
                            }
                        }
                    }
                    // Selective flash did write to flash — return the
                    // deployer so post-deploy recovery runs.
                    return result.map(|r| {
                        let boxed: Box<dyn fbuild_deploy::Deployer> = Box::new(deployer);
                        (Some(boxed), r)
                    });
                }

                // Capture the hash into the boxed deployer closure
                // below — the full-flash path (`deployer.deploy(...)`)
                // at the end of this `spawn_blocking` applies the
                // same record/invalidate rule. We stash the hash +
                // port via a small impl-only wrapper because the
                // `Deployer` trait doesn't expose the cache hook.
                // Stored here so the common tail after the `match`
                // can reach it without re-threading every arm.
                // (AVR / other arms leave it `None` → no-op.)
                trusted_hash_update = image_hash.zip(deploy_port.as_deref().map(str::to_string));

                Box::new(deployer)
            }
            fbuild_core::Platform::AtmelAvr | fbuild_core::Platform::AtmelMegaAvr => {
                let board_config = fbuild_config::BoardConfig::from_board_id_or_default(
                    &board_id,
                    "uno",
                    &deploy_board_overrides,
                    Some(deploy_project.as_path()),
                );
                let avr_config = fbuild_build::avr::mcu_config::get_avr_config().unwrap();
                let avrdude_params = fbuild_deploy::avr::AvrdudeParams {
                    default_programmer: avr_config.avrdude.default_programmer.clone(),
                    default_baud: avr_config.avrdude.default_baud.to_string(),
                    timeout_secs: avr_config.avrdude.timeout_secs,
                };
                let deployer = fbuild_deploy::avr::AvrDeployer::from_board_config(
                    &board_config,
                    &avrdude_params,
                    false,
                );
                let deployer = if let Some(baud) = baud_override {
                    deployer.with_baud_rate(&baud.to_string())
                } else {
                    deployer
                };
                Box::new(deployer)
            }
            fbuild_core::Platform::Teensy => {
                // TeensyDeployer state machine (#433) is dispatched here.
                // Initial wire-up was #430/#431; the deployer itself now owns
                // baud-134 soft reboot, bounded retry, post-flash port
                // discovery, and the optional first-byte advisory probe.
                let board_config = fbuild_config::BoardConfig::from_board_id_or_default(
                    &board_id,
                    "teensy41",
                    &deploy_board_overrides,
                    Some(deploy_project.as_path()),
                );
                let loader_params = fbuild_deploy::teensy::TeensyLoaderParams::default();
                let loader_path = find_teensy_loader_cli();
                let deployer = fbuild_deploy::teensy::TeensyDeployer::new(
                    &board_config.board.to_uppercase(),
                    &loader_params,
                    loader_path,
                    false,
                );
                Box::new(deployer)
            }
            fbuild_core::Platform::NxpLpc => fbuild_deploy::lpc::dispatch_box(&board_id, &deploy_board_overrides, deploy_project.as_path(), baud_override),
            _ => return Err(fbuild_core::FbuildError::DeployFailed(format!("deployer for {:?} not yet implemented", platform))),
        };
        let result = deployer.deploy(
            &deploy_project,
            &deploy_env,
            &deploy_fw,
            deploy_port.as_deref(),
        );
        // Session-trusted verify-skip: record (or invalidate) the
        // image hash the daemon associates with this port. Scoped
        // to the Espressif32 arm above — other platforms leave
        // `trusted_hash_update` as `None` and this block no-ops.
        // Failed or partial deploys clear the cache so the next
        // warm run falls back to the verify-flash path.
        if let Some((hash, port)) = trusted_hash_update {
            match &result {
                Ok(r) if r.success => {
                    ctx_for_deploy
                        .device_manager
                        .set_trusted_firmware_hash(&port, hash);
                }
                _ => {
                    ctx_for_deploy
                        .device_manager
                        .clear_trusted_firmware_hash(&port);
                }
            }
        }
        // Return the deployer so the async caller can invoke
        // `post_deploy_recovery` after `clear_preemption().await` (#605).
        result.map(|r| (Some(deployer), r))
    })
    .await;

    // Split the deployer out so it can drive recovery while `deploy_result`
    // retains its original shape for the downstream match. `None` covers
    // verify-skip early returns, unsupported platforms, and join errors.
    let (deployer_for_recovery, deploy_result) = match deploy_result {
        Ok(Ok((d, r))) => (d, Ok(Ok(r))),
        Ok(Err(e)) => (None, Ok(Err(e))),
        Err(e) => (None, Err(e)),
    };

    // Skip recovery when the deploy didn't touch flash (VerifySkip): no
    // reset, no USB re-enumeration, and the poll's `open()` probe would
    // conflict with any already-attached monitor — load-bearing for the
    // <4 s warm-trust-skip budget.
    let deploy_skipped_bus_work = matches!(
        &deploy_result,
        Ok(Ok(r)) if r.success && matches!(r.outcome, fbuild_deploy::DeployOutcome::VerifySkip)
    );
    if let Some(ref p) = deploy_port_str {
        ctx.serial_manager.clear_preemption(p).await;
        if !deploy_skipped_bus_work {
            if let Some(deployer) = deployer_for_recovery {
                let port_name = p.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(e) = deployer.post_deploy_recovery(&port_name) {
                        tracing::warn!("post_deploy_recovery failed for {}: {}", port_name, e);
                    }
                })
                .await;
            }
        }
    }

    let (deploy_success, deploy_stdout, mut deploy_stderr, deploy_outcome, deploy_post_port) =
        match deploy_result {
            Ok(Ok(r)) if r.success => (true, Some(r.stdout), Some(r.stderr), r.outcome, r.port),
            Ok(Ok(r)) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse {
                        success: false,
                        request_id,
                        message: r.message,
                        exit_code: 1,
                        output_file: Some(reported_output_file.clone()),
                        output_dir: reported_output_dir.clone(),
                        launch_url: None,
                        stdout: Some(r.stdout),
                        stderr: Some(r.stderr),
                    }),
                );
            }
            Ok(Err(e)) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("deploy error: {}", e),
                    )),
                );
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(OperationResponse::fail(
                        request_id,
                        format!("deploy task panicked: {}", e),
                    )),
                );
            }
        };
    append_warning_to_stderr(&mut deploy_stderr, deploy_port_warning);
    // Build the "deploy succeeded (...)" prefix used by every
    // monitor-attached and non-monitor-attached response below. Stable
    // wording — see GitHub issue #76 and the DeployOutcome::describe
    // test in fbuild-deploy.
    let deploy_prefix = format!("deploy succeeded ({})", deploy_outcome.describe());

    // Post-deploy monitoring: if monitor_after is set, open the serial port
    // and stream lines checking halt conditions (matching Python behavior).
    if deploy_success && req.monitor_after {
        // Prefer the post-flash port name surfaced by the deployer (e.g. the
        // Teensy state machine in #433 returns the freshly re-enumerated CDC
        // ACM port, which can differ from the pre-flash `--port`). Fall back
        // to the caller-supplied port, then to a platform default.
        let monitor_port = deploy_post_port
            .clone()
            .or(deploy_port_str.clone())
            .unwrap_or_else(|| "/dev/ttyUSB0".to_string());
        if deploy_post_port
            .as_deref()
            .zip(deploy_port_str.as_deref())
            .is_some_and(|(post, pre)| post != pre)
        {
            tracing::info!(
                "device re-enumerated as {} after flash (was {}); monitor attaching to {}",
                deploy_post_port.as_deref().unwrap_or(""),
                deploy_port_str.as_deref().unwrap_or(""),
                monitor_port,
            );
        }
        let baud_rate = 115200u32;

        // Open the port for monitoring
        if let Err(e) = ctx
            .serial_manager
            .open_port(&monitor_port, baud_rate, &request_id, None)
            .await
        {
            return (
                StatusCode::OK,
                Json(OperationResponse {
                    success: true,
                    request_id,
                    message: format!("{} but monitor failed to open port: {}", deploy_prefix, e),
                    exit_code: 0,
                    output_file: Some(reported_output_file.clone()),
                    output_dir: reported_output_dir.clone(),
                    launch_url: None,
                    stdout: deploy_stdout,
                    stderr: deploy_stderr,
                }),
            );
        }

        // Subscribe to broadcast channel
        let mut rx = match ctx.serial_manager.attach_reader(&monitor_port, &request_id) {
            Some(rx) => rx,
            None => {
                return (
                    StatusCode::OK,
                    Json(OperationResponse {
                        success: true,
                        request_id,
                        message: format!("{} but monitor could not attach reader", deploy_prefix),
                        exit_code: 0,
                        output_file: Some(reported_output_file.clone()),
                        output_dir: reported_output_dir.clone(),
                        launch_url: None,
                        stdout: deploy_stdout,
                        stderr: deploy_stderr,
                    }),
                );
            }
        };

        // #532 fold: post-deploy monitor opts out; collapse unreachable RecoverDownloadMode → Error.
        let monitor_result = match run_monitor_loop(
            &mut rx,
            req.monitor_timeout,
            req.monitor_halt_on_error.as_deref(),
            req.monitor_halt_on_success.as_deref(),
            req.monitor_expect.as_deref(),
            req.monitor_show_timestamp,
            false,
        )
        .await
        {
            MonitorOutcome::RecoverDownloadMode { .. } => {
                unreachable!("post-deploy monitor passes auto_recover_from_download_mode=false")
            }
            other => other,
        };

        ctx.serial_manager.detach_reader(&monitor_port, &request_id);

        return match monitor_result {
            MonitorOutcome::Success(msg) => (
                StatusCode::OK,
                Json(OperationResponse {
                    success: true,
                    request_id,
                    message: format!("{}; monitor: {}", deploy_prefix, msg),
                    exit_code: 0,
                    output_file: Some(reported_output_file.clone()),
                    output_dir: reported_output_dir.clone(),
                    launch_url: None,
                    stdout: deploy_stdout,
                    stderr: deploy_stderr,
                }),
            ),
            MonitorOutcome::Error(msg) => (
                StatusCode::OK,
                Json(OperationResponse {
                    success: false,
                    request_id,
                    message: format!("{}; monitor error: {}", deploy_prefix, msg),
                    exit_code: 1,
                    output_file: Some(reported_output_file.clone()),
                    output_dir: reported_output_dir.clone(),
                    launch_url: None,
                    stdout: deploy_stdout,
                    stderr: deploy_stderr,
                }),
            ),
            // Eliminated by the fold above; the compiler can't narrow the type.
            MonitorOutcome::RecoverDownloadMode { .. } => unreachable!(),
            MonitorOutcome::Timeout { expect_found } => {
                let (success, code) = if expect_found {
                    (true, 0)
                } else {
                    // If expect was set and not found, that's an error
                    (
                        req.monitor_expect.is_none(),
                        if req.monitor_expect.is_none() { 0 } else { 1 },
                    )
                };
                (
                    StatusCode::OK,
                    Json(OperationResponse {
                        success,
                        request_id,
                        message: format!(
                            "{}; monitor timed out{}",
                            deploy_prefix,
                            if !expect_found && req.monitor_expect.is_some() {
                                " (expected pattern not found)"
                            } else {
                                ""
                            }
                        ),
                        exit_code: code,
                        output_file: Some(reported_output_file.clone()),
                        output_dir: reported_output_dir.clone(),
                        launch_url: None,
                        stdout: deploy_stdout,
                        stderr: deploy_stderr,
                    }),
                )
            }
        };
    }

    (
        StatusCode::OK,
        Json(OperationResponse {
            success: true,
            request_id,
            message: deploy_prefix,
            exit_code: 0,
            output_file: Some(reported_output_file),
            output_dir: reported_output_dir,
            launch_url: None,
            stdout: deploy_stdout,
            stderr: deploy_stderr,
        }),
    )
}

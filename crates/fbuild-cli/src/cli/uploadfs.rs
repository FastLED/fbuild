//! `fbuild uploadfs` — flash a filesystem image to the partition it belongs to.
//!
//! The deploy half of FastLED/fbuild#1358. `fbuild buildfs` assembles
//! `<build>/<filesystem>.bin` sized for the partition the firmware mounts;
//! this command writes that image to the offset the same partition CSV
//! declares, through the same `esptool` deployer `fbuild deploy` uses.
//!
//! * the offset and the capacity check come from [`buildfs::plan_image`], so
//!   the two commands cannot disagree about where `data/` ends up;
//! * the image is rebuilt when it is missing (PlatformIO's `uploadfs` implies
//!   `buildfs`), unless `--no-build` asks for the existing file as-is;
//! * a running daemon is asked to preempt the port first, so a monitor session
//!   on the same device does not make the flash fail with "busy";
//! * `--verify` reads the region back with `esptool verify-flash`, proving the
//!   bytes landed rather than only that esptool exited zero.
//!
//! Flashing a filesystem is meaningful only where the platform has one, so a
//! non-ESP32 environment is a clear error instead of a silent no-op.

use std::path::Path;

use clap::Args;
use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};

use super::buildfs::{self, BuildfsArgs, ImagePlan, Partition};
use crate::output;

/// `fbuild uploadfs` arguments.
#[derive(Args, Debug)]
pub struct UploadfsArgs {
    /// Project directory (defaults to the enclosing project).
    pub project_dir: Option<String>,
    /// Target environment.
    #[arg(short = 'e', long)]
    pub environment: Option<String>,
    /// Directory to pack when the image has to be built (defaults to `data/`).
    #[arg(long = "data-dir")]
    pub data_dir: Option<String>,
    /// Partition table CSV (defaults to `board_build.partitions`, else the
    /// framework's `default.csv`).
    #[arg(long)]
    pub partitions: Option<String>,
    /// Filesystem partition label (defaults to the first data partition with a
    /// spiffs/littlefs subtype).
    #[arg(long = "fs-partition")]
    pub fs_partition: Option<String>,
    /// Image format for a rebuild: `littlefs` or `spiffs`.
    #[arg(long)]
    pub filesystem: Option<String>,
    /// Path to `mklittlefs`/`mkspiffs` for a rebuild.
    #[arg(long)]
    pub tool: Option<String>,
    /// Prebuilt image to flash instead of the build directory's image.
    #[arg(short = 'o', long = "image")]
    pub image: Option<String>,
    /// Flash the existing image without rebuilding a missing one.
    #[arg(long = "no-build")]
    pub no_build: bool,
    /// Serial port (defaults to the only detected port).
    #[arg(short, long)]
    pub port: Option<String>,
    /// Upload baud rate (defaults to the board's `upload_speed`).
    #[arg(long)]
    pub baud: Option<u32>,
    /// Read the region back after writing it.
    #[arg(long)]
    pub verify: bool,
    /// Emit a machine-readable JSON summary.
    #[arg(long)]
    pub json: bool,
}

impl UploadfsArgs {
    /// The `buildfs` invocation an `uploadfs` implies: same environment, same
    /// data directory, same partition table, same output path.
    fn build_args(&self) -> BuildfsArgs {
        BuildfsArgs {
            project_dir: self.project_dir.clone(),
            environment: self.environment.clone(),
            data_dir: self.data_dir.clone(),
            partitions: self.partitions.clone(),
            fs_partition: self.fs_partition.clone(),
            filesystem: self.filesystem.clone(),
            tool: self.tool.clone(),
            output: self.image.clone(),
            check_only: false,
            json: false,
        }
    }
}

fn absolute(project_dir: &Path, value: &str) -> NormalizedPath {
    let path = Path::new(value);
    if path.is_absolute() {
        NormalizedPath::new(path)
    } else {
        NormalizedPath::new(project_dir.join(path))
    }
}

/// Reject environments whose platform has no flash filesystem.
///
/// ESP32 is the only platform fbuild builds images for; AVR has just EEPROM and
/// Teensy 4.x's `LittleFS_Program` is runtime-only (re-uploading wipes it), so
/// there is nothing correct to flash there and saying so beats a silent no-op.
fn require_flash_filesystem(platform: Option<&str>, env: &str) -> Result<()> {
    match platform.and_then(fbuild_core::Platform::from_platform_str) {
        Some(fbuild_core::Platform::Espressif32) => Ok(()),
        Some(platform) => Err(FbuildError::ConfigError(format!(
            "`fbuild uploadfs` flashes an ESP32 flash-filesystem partition built from an ESP32 \
             partition table; environment '{env}' targets {platform:?}, which fbuild does not \
             build filesystem images for (AVR has only EEPROM, Teensy 4.x's LittleFS_Program is \
             runtime-only and wiped by every re-upload, ESP8266 is not implemented)"
        ))),
        None => Err(FbuildError::ConfigError(format!(
            "environment '{env}' declares no supported platform; `fbuild uploadfs` needs an \
             ESP32 environment"
        ))),
    }
}

/// Pick the port to flash: the only one, or a clear error naming the choices.
fn choose_port(ports: &[String]) -> Result<String> {
    match ports {
        [only] => Ok(only.clone()),
        [] => Err(FbuildError::SerialError(
            "no serial port detected; connect the board or pass --port".to_string(),
        )),
        many => Err(FbuildError::SerialError(format!(
            "{} serial ports detected ({}); pass --port to choose the board to flash",
            many.len(),
            many.join(", ")
        ))),
    }
}

fn detected_port_names() -> Result<Vec<String>> {
    let ports = fbuild_serial::ports::available_ports()
        .map_err(|e| FbuildError::SerialError(format!("failed to enumerate serial ports: {e}")))?;
    Ok(ports.into_iter().map(|port| port.info.port_name).collect())
}

/// Ask a running daemon to release the port before we open it ourselves.
///
/// The daemon is the serial-lease owner; a monitor session it still holds would
/// make esptool fail with "port busy". Best-effort: with no daemon running there
/// is nothing to preempt.
async fn preempt_running_daemon(port: &str) {
    let client = crate::daemon_client::DaemonClient::new();
    if !client.health().await {
        return;
    }
    if let Err(error) = client.device_preempt(port, "uploadfs").await {
        output::warn(format!(
            "could not preempt {port} from the running daemon ({error}); \
             a monitor session may still hold the port"
        ));
    }
}

/// Build the ESP32 deployer `fbuild deploy` would use, so `uploadfs` flashes
/// with the board's own flash mode/frequency/size and the build's esptool.
async fn esp32_deployer(
    project_dir: &Path,
    plan: &ImagePlan,
    baud: Option<u32>,
) -> Result<fbuild_deploy::esp32::Esp32Deployer> {
    let board_id = plan
        .context
        .env_config
        .get("board")
        .cloned()
        .unwrap_or_else(|| "esp32dev".to_string());
    let board = fbuild_config::BoardConfig::from_board_id_or_default(
        &board_id,
        "esp32dev",
        &plan.context.board_overrides,
        Some(project_dir),
    );
    let mcu_config = fbuild_build::esp32::mcu_config::get_mcu_config(&board.mcu).map_err(|e| {
        FbuildError::ConfigError(format!(
            "unsupported ESP32 MCU '{}' for board '{}': {}",
            board.mcu, board_id, e
        ))
    })?;
    let esptool_params = fbuild_deploy::esp32::EsptoolParams {
        flash_mode: board
            .flash_mode
            .as_deref()
            .unwrap_or(mcu_config.default_flash_mode())
            .to_string(),
        flash_freq: {
            let f_for_image = board.f_image.as_deref().or(board.f_flash.as_deref());
            fbuild_build::esp32::esp32_linker::f_flash_to_esptool_freq(
                f_for_image,
                mcu_config.default_flash_freq(),
            )
        },
        flash_size: fbuild_build::esp32::mcu_config::bytes_to_flash_size(
            board.max_flash,
            mcu_config.default_flash_size(),
        )
        .to_string(),
        default_baud: mcu_config.default_baud().to_string(),
        before_reset: mcu_config.before_reset().to_string(),
        after_reset: mcu_config.after_reset().to_string(),
    };
    let esptool_path = match fbuild_build::esp32::orchestrator::resolve_deploy_esptool(
        project_dir,
        Some(&plan.context.env_config),
    )
    .await
    {
        Ok(path) => path,
        Err(error) => {
            output::warn(format!(
                "could not resolve the build's esptool ({error}); falling back to `esptool` on PATH"
            ));
            None
        }
    };
    let mut deployer = fbuild_deploy::esp32::Esp32Deployer::from_board_config(
        &board,
        mcu_config.bootloader_offset(),
        mcu_config.partitions_offset(),
        mcu_config.firmware_offset(),
        &esptool_params,
        false,
    )
    .with_esptool_path(esptool_path)
    .with_caller_path(std::env::var("PATH").ok());
    if let Some(baud) = baud {
        deployer = deployer.with_baud_rate(&baud.to_string());
    }
    Ok(deployer)
}

/// Resolve, build (if needed) and flash the filesystem image.
pub async fn run(args: UploadfsArgs, project_dir: &Path) -> Result<()> {
    let plan = buildfs::plan_image(&args.build_args(), project_dir)?;
    require_flash_filesystem(
        plan.context.env_config.get("platform").map(String::as_str),
        &plan.context.env,
    )?;

    let image = match &args.image {
        Some(path) => absolute(project_dir, path),
        None => plan.image.clone(),
    };
    if !image.is_file() {
        if args.no_build {
            return Err(FbuildError::ConfigError(format!(
                "no filesystem image at {}; drop --no-build to build it first",
                image.as_path().display()
            )));
        }
        output::progress(format!(
            "no image at {}; building it first",
            image.as_path().display()
        ));
        buildfs::run(args.build_args(), project_dir).await?;
        if !image.is_file() {
            return Err(FbuildError::BuildFailed(format!(
                "buildfs did not produce {}",
                image.as_path().display()
            )));
        }
    }

    let image_bytes = std::fs::metadata(image.as_path())
        .map_err(|e| {
            FbuildError::BuildFailed(format!("failed to stat {}: {e}", image.as_path().display()))
        })?
        .len();
    if image_bytes > plan.partition.size {
        return Err(FbuildError::ConfigError(format!(
            "image {} is {} bytes but the '{}' partition holds {}; rebuild it with `fbuild buildfs`",
            image.as_path().display(),
            image_bytes,
            plan.partition.label,
            plan.partition.size
        )));
    }

    let port = match fbuild_serial::port_selector::resolve_port_arg(args.port.clone())? {
        Some(port) => port,
        None => choose_port(&detected_port_names()?)?,
    };
    preempt_running_daemon(&port).await;

    let deployer = esp32_deployer(project_dir, &plan, args.baud).await?;
    let offset = format!("0x{:x}", plan.partition.offset);
    output::progress(format!(
        "flashing {} ({} bytes) to '{}' @ {} on {}",
        image.as_path().display(),
        image_bytes,
        plan.partition.label,
        offset,
        port
    ));
    let written = deployer
        .flash_region(image.as_path(), &port, &offset)
        .await?;
    if !written.success {
        return Err(FbuildError::DeployFailed(format!(
            "esptool write-flash failed (exit code {}):\n{}{}",
            written.exit_code, written.stderr, written.stdout
        )));
    }

    let verified = if args.verify {
        let check = deployer
            .verify_region(image.as_path(), &port, &offset)
            .await?;
        if !check.success {
            return Err(FbuildError::DeployFailed(format!(
                "verify-flash did not match {} at {} (exit code {}):\n{}{}",
                image.as_path().display(),
                offset,
                check.exit_code,
                check.stderr,
                check.stdout
            )));
        }
        true
    } else {
        false
    };

    report(&UploadReport {
        args: &args,
        plan: &plan,
        image: &image,
        image_bytes,
        port: &port,
        offset: &offset,
        verified,
    });
    Ok(())
}

struct UploadReport<'a> {
    args: &'a UploadfsArgs,
    plan: &'a ImagePlan,
    image: &'a NormalizedPath,
    image_bytes: u64,
    port: &'a str,
    offset: &'a str,
    verified: bool,
}

fn report(report: &UploadReport<'_>) {
    let partition: &Partition = &report.plan.partition;
    if report.args.json {
        let summary = serde_json::json!({
            "ok": true,
            "filesystem": report.plan.kind.as_str(),
            "environment": report.plan.context.env,
            "partition": {
                "label": partition.label,
                "offset": partition.offset,
                "size": partition.size,
            },
            "image": report.image.as_path().display().to_string(),
            "image_bytes": report.image_bytes,
            "port": report.port,
            "offset_hex": report.offset,
            "verified": report.verified,
        });
        output::result(serde_json::to_string_pretty(&summary).unwrap_or_default());
        return;
    }
    output::result(format!(
        "flashed {} {} ({} bytes) to '{}' @ {} on {}",
        report.plan.kind.as_str(),
        report.image.as_path().display(),
        report.image_bytes,
        partition.label,
        report.offset,
        report.port
    ));
    if report.verified {
        output::result(format!("verified: {} matches the device", report.offset));
    } else {
        output::result("not verified (pass --verify to read the region back)");
    }
}

#[cfg(test)]
#[path = "uploadfs_tests.rs"]
mod tests;

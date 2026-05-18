//! `EmulatorRunner` trait and three concrete runners (`QemuRunner`,
//! `Avr8jsRunner`, `SimavrRunner`) used by `select_runner` / `test_emu`.

use super::avr8js_headless::{run_avr8js_headless, RunAvr8jsHeadlessOptions, AVR8JS_HEADLESS_MJS};
use super::avr8js_npm::{ensure_avr8js_npm, find_node};
use super::qemu_deploy::resolve_esp_qemu_for_mcu;
use super::shared::{
    monitor_outcome_to_emulator, qemu_session_dir, resolve_esp32_toolchain_gcc_path,
    run_qemu_process, EmulatorRunConfig, RunQemuOptions,
};
use fbuild_core::emulator::{EmulatorOutcome, EmulatorRunResult};
use std::path::PathBuf;

/// Abstraction over emulator backends. Each implementation knows how to set up
/// and execute a specific emulator (QEMU, avr8js, etc.).
#[async_trait::async_trait]
pub trait EmulatorRunner: Send + Sync {
    /// Human-readable name of this runner (e.g. "QEMU ESP32-S3", "avr8js ATmega328P").
    fn name(&self) -> &str;

    /// Run the emulator with the given configuration.
    async fn run(&self, config: &EmulatorRunConfig) -> fbuild_core::Result<EmulatorRunResult>;
}

/// QEMU-based emulator runner for ESP32-family boards.
pub struct QemuRunner {
    project_dir: PathBuf,
    env_name: String,
    board: fbuild_config::BoardConfig,
    display_name: String,
}

impl QemuRunner {
    pub fn new(project_dir: PathBuf, env_name: String, board: fbuild_config::BoardConfig) -> Self {
        let display_name = format!("QEMU {}", board.mcu.to_uppercase());
        Self {
            project_dir,
            env_name,
            board,
            display_name,
        }
    }
}

#[async_trait::async_trait]
impl EmulatorRunner for QemuRunner {
    fn name(&self) -> &str {
        &self.display_name
    }

    async fn run(&self, config: &EmulatorRunConfig) -> fbuild_core::Result<EmulatorRunResult> {
        if let Err(msg) = config
            .artifact_bundle
            .validate_for(fbuild_core::emulator::RunnerKind::QemuEsp32)
        {
            return Err(fbuild_core::FbuildError::DeployFailed(msg));
        }
        let mcu_config = fbuild_build::esp32::mcu_config::get_mcu_config(&self.board.mcu)?;

        let effective_flash_mode = self
            .board
            .flash_mode
            .as_deref()
            .unwrap_or(mcu_config.default_flash_mode());
        if !effective_flash_mode.eq_ignore_ascii_case("dio") {
            return Ok(EmulatorRunResult {
                outcome: EmulatorOutcome::Unsupported(format!(
                    "QEMU requires DIO flash mode; effective mode is '{}'",
                    effective_flash_mode
                )),
                stdout: String::new(),
                stderr: String::new(),
                command_line: String::new(),
                exit_code: None,
            });
        }

        let flash_size_bytes = fbuild_deploy::esp32::resolve_qemu_flash_size_bytes(
            &self.board,
            mcu_config.default_flash_size(),
        )?;

        let qemu = resolve_esp_qemu_for_mcu(&self.project_dir, &self.board.mcu)?;

        let session_dir = qemu_session_dir(&self.project_dir, &self.env_name);
        std::fs::create_dir_all(&session_dir)?;

        let flash_image = session_dir.join("qemu_flash.bin");

        // Only apply the ESP32-S3 ADC calibration patch for S3 variants.
        let elf_for_adc_patch = if self.board.mcu.eq_ignore_ascii_case("esp32s3") {
            config.elf_path.as_deref()
        } else {
            None
        };
        fbuild_deploy::esp32::create_qemu_flash_image(
            &config.firmware_path,
            &flash_image,
            flash_size_bytes,
            mcu_config.bootloader_offset(),
            mcu_config.partitions_offset(),
            mcu_config.firmware_offset(),
            elf_for_adc_patch,
        )?;

        let args = fbuild_deploy::esp32::build_qemu_args(
            &self.board.mcu,
            &flash_image,
            self.board.qemu_esp32_psram_config(),
        );
        let addr2line_path = config.elf_path.as_ref().and_then(|_| {
            resolve_esp32_toolchain_gcc_path(&self.project_dir, &mcu_config)
                .ok()
                .and_then(|gcc| fbuild_serial::crash_decoder::derive_addr2line_path(&gcc))
        });

        let command_line = format!("{} {}", qemu.display(), args.join(" "));

        let qemu_result = run_qemu_process(
            &qemu,
            &args,
            RunQemuOptions {
                elf_path: config.elf_path.clone(),
                addr2line_path,
                timeout_secs: config.timeout,
                halt_on_error: config.halt_on_error.as_deref(),
                halt_on_success: config.halt_on_success.as_deref(),
                expect: config.expect.as_deref(),
                show_timestamp: config.show_timestamp,
                verbose: config.verbose,
                process_label: "QEMU",
            },
        )
        .await?;

        let exit_code = qemu_result.exit_code;
        let outcome = monitor_outcome_to_emulator(qemu_result.outcome, exit_code);

        Ok(EmulatorRunResult {
            outcome,
            stdout: qemu_result.stdout,
            stderr: qemu_result.stderr,
            command_line,
            exit_code,
        })
    }
}

/// AVR8js-based emulator runner for ATmega328P (headless Node.js).
pub struct Avr8jsRunner {
    board: fbuild_config::BoardConfig,
}

impl Avr8jsRunner {
    pub fn new(board: fbuild_config::BoardConfig) -> Self {
        Self { board }
    }
}

#[async_trait::async_trait]
impl EmulatorRunner for Avr8jsRunner {
    fn name(&self) -> &str {
        "avr8js ATmega328P"
    }

    async fn run(&self, config: &EmulatorRunConfig) -> fbuild_core::Result<EmulatorRunResult> {
        if let Err(msg) = config
            .artifact_bundle
            .validate_for(fbuild_core::emulator::RunnerKind::Avr8js)
        {
            return Err(fbuild_core::FbuildError::DeployFailed(msg));
        }
        let node_path = find_node()?;
        let avr8js_cache = ensure_avr8js_npm()?;

        let session_dir = tempfile::TempDir::new()?;
        let script_path = session_dir.path().join("headless.mjs");
        std::fs::write(&script_path, AVR8JS_HEADLESS_MJS)?;

        let f_cpu_hz: u32 = self
            .board
            .f_cpu
            .trim_end_matches('L')
            .parse()
            .unwrap_or(16_000_000);

        let command_line = format!(
            "{} {} --hex {} --f-cpu {}",
            node_path.display(),
            script_path.display(),
            config.firmware_path.display(),
            f_cpu_hz
        );

        let avr8js_result = run_avr8js_headless(
            &node_path,
            &script_path,
            &config.firmware_path,
            f_cpu_hz,
            &avr8js_cache,
            RunAvr8jsHeadlessOptions {
                timeout_secs: config.timeout,
                halt_on_error: config.halt_on_error.as_deref(),
                halt_on_success: config.halt_on_success.as_deref(),
                expect: config.expect.as_deref(),
                show_timestamp: config.show_timestamp,
                verbose: config.verbose,
            },
        )
        .await?;

        let exit_code = avr8js_result.exit_code;
        let outcome = monitor_outcome_to_emulator(avr8js_result.outcome, exit_code);

        Ok(EmulatorRunResult {
            outcome,
            stdout: avr8js_result.stdout,
            stderr: avr8js_result.stderr,
            command_line,
            exit_code,
        })
    }
}

/// Find the `simavr` binary on PATH.
///
/// SimAVR is a native AVR simulator. Install via:
/// - Linux: `apt install simavr` or build from source
/// - macOS: `brew install simavr`
/// - Windows: build from source (MSYS2/MinGW) — limited support
fn find_simavr() -> fbuild_core::Result<PathBuf> {
    let simavr = if cfg!(windows) {
        "simavr.exe"
    } else {
        "simavr"
    };
    // Try running simavr to verify it exists; route through containment
    // (issue #32). This is a short-lived probe so the containment
    // difference is purely consistency.
    match fbuild_core::subprocess::run_command(&[simavr, "--help"], None, None, None) {
        Ok(_) => Ok(PathBuf::from(simavr)),
        Err(_) => {
            let install_hint = if cfg!(target_os = "linux") {
                "Install via: apt install simavr (Debian/Ubuntu) or your distro's package manager"
            } else if cfg!(target_os = "macos") {
                "Install via: brew install simavr"
            } else {
                "SimAVR has limited Windows support. Build from source via MSYS2/MinGW, \
                 or use --emulator avr8js for ATmega328P boards instead"
            };
            Err(fbuild_core::FbuildError::DeployFailed(format!(
                "simavr is required but '{}' was not found on PATH. {}",
                simavr, install_hint
            )))
        }
    }
}

/// SimAVR-based native emulator runner for AVR boards.
///
/// Supports any AVR board that advertises `simavr` in its debug_tools.
/// Primary targets: ATmega328P (Uno), ATmega32U4 (Leonardo), ATmega2560 (Mega).
/// Consumes `firmware.elf` directly.
pub struct SimavrRunner {
    board: fbuild_config::BoardConfig,
}

impl SimavrRunner {
    pub fn new(board: fbuild_config::BoardConfig) -> Self {
        Self { board }
    }
}

#[async_trait::async_trait]
impl EmulatorRunner for SimavrRunner {
    fn name(&self) -> &str {
        "simavr"
    }

    async fn run(&self, config: &EmulatorRunConfig) -> fbuild_core::Result<EmulatorRunResult> {
        if let Err(msg) = config
            .artifact_bundle
            .validate_for(fbuild_core::emulator::RunnerKind::Simavr)
        {
            return Err(fbuild_core::FbuildError::DeployFailed(msg));
        }
        let simavr_path = find_simavr()?;

        // simavr requires an ELF file
        let elf_path = config.elf_path.as_ref().ok_or_else(|| {
            fbuild_core::FbuildError::DeployFailed(
                "simavr requires firmware.elf but no ELF path was produced by the build"
                    .to_string(),
            )
        })?;

        let f_cpu_hz: u32 = self
            .board
            .f_cpu
            .trim_end_matches('L')
            .parse()
            .unwrap_or(16_000_000);

        let mcu = self.board.mcu.to_lowercase();

        let args: Vec<String> = vec![
            "-m".to_string(),
            mcu.clone(),
            "-f".to_string(),
            f_cpu_hz.to_string(),
            elf_path.display().to_string(),
        ];

        let command_line = format!("{} {}", simavr_path.display(), args.join(" "));

        // Reuse the generic process runner (run_qemu_process) with no crash decoder
        let result = run_qemu_process(
            &simavr_path,
            &args,
            RunQemuOptions {
                elf_path: None,       // no ESP32 crash decoder needed
                addr2line_path: None, // no addr2line for AVR in this path
                timeout_secs: config.timeout,
                halt_on_error: config.halt_on_error.as_deref(),
                halt_on_success: config.halt_on_success.as_deref(),
                expect: config.expect.as_deref(),
                show_timestamp: config.show_timestamp,
                verbose: config.verbose,
                process_label: "simavr",
            },
        )
        .await?;

        let exit_code = result.exit_code;
        let outcome = monitor_outcome_to_emulator(result.outcome, exit_code);

        Ok(EmulatorRunResult {
            outcome,
            stdout: result.stdout,
            stderr: result.stderr,
            command_line,
            exit_code,
        })
    }
}

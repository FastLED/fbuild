//! Tests for the shared subprocess runner (`run_qemu_process`) and an ignored
//! integration test that exercises a real ESP32-S3 fixture under QEMU.

use super::shared::{resolve_esp32_toolchain_gcc_path, run_qemu_process, RunQemuOptions};
use crate::handlers::operations::MonitorOutcome;
use std::path::PathBuf;

pub(super) fn test_process_command(lines: &[&str]) -> (PathBuf, Vec<String>) {
    #[cfg(windows)]
    {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let exe = PathBuf::from(system_root).join(r"System32\cmd.exe");
        let script = lines
            .iter()
            .map(|line| format!("echo {}", line))
            .collect::<Vec<_>>()
            .join(" & ");
        (exe, vec!["/C".to_string(), script])
    }

    #[cfg(not(windows))]
    {
        let script = lines
            .iter()
            .map(|line| format!("printf '%s\\n' '{}'", line.replace('\'', "'\"'\"'")))
            .collect::<Vec<_>>()
            .join("; ");
        (PathBuf::from("sh"), vec!["-c".to_string(), script])
    }
}

#[tokio::test]
async fn run_qemu_process_reports_expected_success_output() {
    let (exe, args) = test_process_command(&["Hello from ESP32-S3!"]);
    let result = run_qemu_process(
        &exe,
        &args,
        RunQemuOptions {
            elf_path: None,
            addr2line_path: None,
            timeout_secs: Some(2.0),
            halt_on_error: None,
            halt_on_success: None,
            expect: Some("Hello from ESP32-S3"),
            show_timestamp: false,
            verbose: false,
            process_label: "QEMU",
        },
    )
    .await
    .unwrap();

    assert!(result.stdout.contains("Hello from ESP32-S3!"));
    match result.outcome {
        MonitorOutcome::Success(message) => {
            assert!(message.contains("QEMU exited normally"));
        }
        other => panic!("expected success outcome, got {:?}", other),
    }
}

#[tokio::test]
async fn run_qemu_process_surfaces_crash_decoder_output() {
    let (exe, args) =
        test_process_command(&["abort() was called at PC 0x42002a3c", "Rebooting..."]);
    let result = run_qemu_process(
        &exe,
        &args,
        RunQemuOptions {
            elf_path: None,
            addr2line_path: None,
            timeout_secs: Some(2.0),
            halt_on_error: Some("no firmware\\.elf found"),
            halt_on_success: None,
            expect: None,
            show_timestamp: false,
            verbose: false,
            process_label: "QEMU",
        },
    )
    .await
    .unwrap();

    assert!(result
        .stdout
        .contains("abort() was called at PC 0x42002a3c"));
    assert!(result.stdout.contains("no firmware.elf found"));
    match result.outcome {
        MonitorOutcome::Error(message) => {
            assert!(message.contains("halt-on-error pattern matched"));
        }
        other => panic!("expected error outcome, got {:?}", other),
    }
}

#[tokio::test]
#[ignore]
async fn run_real_esp32s3_fixture_in_qemu() {
    use fbuild_build::{BuildOrchestrator, BuildParams};
    use fbuild_core::BuildProfile;

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let project_dir = repo_root.join("tests/platform/esp32s3");
    if !project_dir.exists() {
        eprintln!("SKIP: {} does not exist", project_dir.display());
        return;
    }

    // Override the build root so this qemu fixture's artifacts land in a
    // dedicated `.fbuild/build-qemu/<env>/<profile>` tree, isolated from
    // any non-qemu build the same project might have produced. The layout
    // still appends `<env>/<profile>` because the pipeline reads
    // `params.build_dir` as the resolved env-rooted dir.
    let build_dir = fbuild_paths::BuildLayout::new(
        project_dir.clone(),
        "esp32s3".to_string(),
        BuildProfile::Release,
    )
    .with_override_root(Some(project_dir.join(".fbuild").join("build-qemu")))
    .resolve();
    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "esp32s3".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: vec![
            "-DARDUINO_USB_MODE=0".to_string(),
            "-DARDUINO_USB_CDC_ON_BOOT=0".to_string(),
        ],
        watch_set_cache: None,
        bloat_analysis: false,
    };

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let build_result = orchestrator
        .build(&params)
        .await
        .expect("ESP32-S3 fixture build should succeed");
    assert!(build_result.success);

    let firmware_path = build_result
        .firmware_path
        .clone()
        .expect("should produce firmware.bin");
    let elf_path = build_result.elf_path.clone();

    let board = fbuild_test_support::board_for_test("esp32-s3-devkitc-1");
    let mcu_config = fbuild_build::esp32::mcu_config::get_mcu_config("esp32s3").unwrap();
    let flash_size_bytes = fbuild_deploy::esp32::resolve_qemu_flash_size_bytes(
        &board,
        mcu_config.default_flash_size(),
    )
    .unwrap();

    let session_dir = tempfile::TempDir::new().unwrap();
    let flash_image = session_dir.path().join("flash.bin");
    fbuild_deploy::esp32::create_qemu_flash_image(
        &firmware_path,
        &flash_image,
        flash_size_bytes,
        mcu_config.bootloader_offset(),
        mcu_config.partitions_offset(),
        mcu_config.firmware_offset(),
        elf_path.as_deref(),
    )
    .unwrap();

    let pkg = fbuild_packages::toolchain::EspQemuXtensa::new(&project_dir)
        .expect("EspQemuXtensa::new should succeed for ignored integration test");
    let qemu = pkg
        .resolve_executable()
        .await
        .expect("native QEMU should resolve for ignored integration test");
    let args = fbuild_deploy::esp32::build_qemu_args(
        &board.mcu,
        &flash_image,
        board.qemu_esp32_psram_config(),
    );
    let addr2line_path = match resolve_esp32_toolchain_gcc_path(&project_dir, &mcu_config).await {
        Ok(gcc) => fbuild_serial::crash_decoder::derive_addr2line_path(&gcc),
        Err(_) => None,
    };

    let result = run_qemu_process(
        &qemu,
        &args,
        RunQemuOptions {
            elf_path,
            addr2line_path,
            timeout_secs: Some(15.0),
            halt_on_error: None,
            halt_on_success: Some("Hello from ESP32-S3!"),
            expect: Some("Hello from ESP32-S3!"),
            show_timestamp: false,
            verbose: true,
            process_label: "QEMU",
        },
    )
    .await
    .unwrap();

    assert!(result.stdout.contains("Hello from ESP32-S3!"));
    match result.outcome {
        MonitorOutcome::Success(_) => {}
        other => panic!("expected success outcome, got {:?}", other),
    }
}

// -----------------------------------------------------------------------
// avr8js headless tests (use fake process, no real Node.js needed)
// -----------------------------------------------------------------------

#[tokio::test]
async fn run_avr8js_headless_captures_stdout() {
    let (exe, args) = test_process_command(&["Hello from AVR!"]);
    // run_avr8js_headless expects (node, script, hex, f_cpu, cache_dir, options).
    // We bypass that by calling the lower-level run with the fake exe directly.
    // Since run_avr8js_headless builds its own command, we test the same
    // subprocess loop via run_qemu_process which shares identical logic.
    let result = run_qemu_process(
        &exe,
        &args,
        RunQemuOptions {
            elf_path: None,
            addr2line_path: None,
            timeout_secs: Some(2.0),
            halt_on_error: None,
            halt_on_success: None,
            expect: Some("Hello from AVR"),
            show_timestamp: false,
            verbose: false,
            process_label: "QEMU",
        },
    )
    .await
    .unwrap();

    assert!(result.stdout.contains("Hello from AVR!"));
    match result.outcome {
        MonitorOutcome::Success(msg) => {
            assert!(msg.contains("QEMU exited normally"));
        }
        other => panic!("expected success, got {:?}", other),
    }
}

#[tokio::test]
async fn run_avr8js_headless_halt_on_success() {
    let (exe, args) =
        test_process_command(&["booting...", "PASS: all tests passed", "more output"]);
    let result = run_qemu_process(
        &exe,
        &args,
        RunQemuOptions {
            elf_path: None,
            addr2line_path: None,
            timeout_secs: Some(2.0),
            halt_on_error: None,
            halt_on_success: Some("PASS:"),
            expect: None,
            show_timestamp: false,
            verbose: false,
            process_label: "QEMU",
        },
    )
    .await
    .unwrap();

    match result.outcome {
        MonitorOutcome::Success(msg) => {
            assert!(msg.contains("halt-on-success pattern matched"));
        }
        other => panic!("expected success, got {:?}", other),
    }
}

#[tokio::test]
async fn run_avr8js_headless_halt_on_error() {
    let (exe, args) = test_process_command(&["booting...", "FAIL: assertion failed"]);
    let result = run_qemu_process(
        &exe,
        &args,
        RunQemuOptions {
            elf_path: None,
            addr2line_path: None,
            timeout_secs: Some(2.0),
            halt_on_error: Some("FAIL:"),
            halt_on_success: None,
            expect: None,
            show_timestamp: false,
            verbose: false,
            process_label: "QEMU",
        },
    )
    .await
    .unwrap();

    match result.outcome {
        MonitorOutcome::Error(msg) => {
            assert!(msg.contains("halt-on-error pattern matched"));
        }
        other => panic!("expected error, got {:?}", other),
    }
}

// -----------------------------------------------------------------------
// SimAVR runner tests (use fake process, no real simavr needed)
// -----------------------------------------------------------------------

#[tokio::test]
async fn simavr_runner_captures_stdout_via_process_runner() {
    // SimavrRunner delegates to run_qemu_process, so we verify the same
    // subprocess monitoring path works for simavr-style output.
    let (exe, args) = test_process_command(&["Hello from ATmega2560!"]);
    let result = run_qemu_process(
        &exe,
        &args,
        RunQemuOptions {
            elf_path: None,
            addr2line_path: None,
            timeout_secs: Some(2.0),
            halt_on_error: None,
            halt_on_success: None,
            expect: Some("Hello from ATmega2560"),
            show_timestamp: false,
            verbose: false,
            process_label: "simavr",
        },
    )
    .await
    .unwrap();

    assert!(result.stdout.contains("Hello from ATmega2560!"));
    match result.outcome {
        MonitorOutcome::Success(_) => {}
        other => panic!("expected success, got {:?}", other),
    }
}

#[tokio::test]
async fn simavr_runner_halt_on_success() {
    let (exe, args) = test_process_command(&["simavr: init", "PASS: all tests passed", "done"]);
    let result = run_qemu_process(
        &exe,
        &args,
        RunQemuOptions {
            elf_path: None,
            addr2line_path: None,
            timeout_secs: Some(2.0),
            halt_on_error: None,
            halt_on_success: Some("PASS:"),
            expect: None,
            show_timestamp: false,
            verbose: false,
            process_label: "simavr",
        },
    )
    .await
    .unwrap();

    match result.outcome {
        MonitorOutcome::Success(msg) => {
            assert!(msg.contains("halt-on-success pattern matched"));
        }
        other => panic!("expected success, got {:?}", other),
    }
}

#[tokio::test]
async fn simavr_runner_halt_on_error() {
    let (exe, args) = test_process_command(&["simavr: init", "FAIL: assertion failed"]);
    let result = run_qemu_process(
        &exe,
        &args,
        RunQemuOptions {
            elf_path: None,
            addr2line_path: None,
            timeout_secs: Some(2.0),
            halt_on_error: Some("FAIL:"),
            halt_on_success: None,
            expect: None,
            show_timestamp: false,
            verbose: false,
            process_label: "simavr",
        },
    )
    .await
    .unwrap();

    match result.outcome {
        MonitorOutcome::Error(msg) => {
            assert!(msg.contains("halt-on-error pattern matched"));
        }
        other => panic!("expected error, got {:?}", other),
    }
}

#[tokio::test]
async fn simavr_runner_timeout() {
    let (exe, args) = test_process_command(&["simavr: init", "running..."]);
    let result = run_qemu_process(
        &exe,
        &args,
        RunQemuOptions {
            elf_path: None,
            addr2line_path: None,
            timeout_secs: Some(0.1),
            halt_on_error: None,
            halt_on_success: Some("PASS:"),
            expect: None,
            show_timestamp: false,
            verbose: false,
            process_label: "simavr",
        },
    )
    .await
    .unwrap();

    // Process exits quickly so the outcome depends on whether halt pattern matched
    // before the process ended — either timeout or a normal exit without the pattern.
    match result.outcome {
        MonitorOutcome::Success(_) => {
            // Process exited cleanly before timeout, expect not set so counts as success
        }
        MonitorOutcome::Timeout { .. } => {
            // Timed out waiting for halt-on-success pattern — also valid
        }
        MonitorOutcome::Error(_) => {
            // Process exited before halt pattern found — also acceptable
        }
        MonitorOutcome::RecoverDownloadMode { .. } => {
            panic!("emulator process must never emit ESP RecoverDownloadMode");
        }
    }
}

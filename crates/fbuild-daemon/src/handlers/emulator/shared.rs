//! Process runner primitives shared by every emulator backend (QEMU, simavr,
//! avr8js headless).
//!
//! Owns the streaming subprocess loop (`run_qemu_process`), line-reader task,
//! shared option/result structs, and a few small helpers used across runners
//! (`qemu_session_dir`, ESP32 toolchain GCC resolution, Windows process
//! flags, `monitor_outcome_to_emulator`).

use crate::handlers::operations::{MonitorOutcome, MonitorState};
use fbuild_core::channel::{unbounded, UnboundedSender};
use fbuild_core::emulator::EmulatorOutcome;
use fbuild_packages::{Package, Toolchain};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

pub(crate) struct ProcessLine {
    pub is_stderr: bool,
    pub line: String,
}

pub(crate) enum ProcessEvent {
    Line(ProcessLine),
    StreamClosed,
}

pub(crate) struct QemuRunResult {
    pub outcome: MonitorOutcome,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

pub(crate) struct RunQemuOptions<'a> {
    pub elf_path: Option<PathBuf>,
    pub addr2line_path: Option<PathBuf>,
    pub timeout_secs: Option<f64>,
    pub halt_on_error: Option<&'a str>,
    pub halt_on_success: Option<&'a str>,
    pub expect: Option<&'a str>,
    pub show_timestamp: bool,
    pub verbose: bool,
    /// Label used in user-visible messages (e.g. "QEMU", "simavr").
    pub process_label: &'a str,
}

/// Configuration for an emulator test run (user-facing options).
pub struct EmulatorRunConfig {
    pub firmware_path: PathBuf,
    pub elf_path: Option<PathBuf>,
    /// Structured artifact bundle for runner validation.
    pub artifact_bundle: fbuild_core::emulator::EmulatorArtifactBundle,
    pub timeout: Option<f64>,
    pub halt_on_error: Option<String>,
    pub halt_on_success: Option<String>,
    pub expect: Option<String>,
    pub show_timestamp: bool,
    pub verbose: bool,
}

/// Convert a `MonitorOutcome` into an `EmulatorOutcome`.
pub(crate) fn monitor_outcome_to_emulator(
    outcome: MonitorOutcome,
    exit_code: Option<i32>,
) -> EmulatorOutcome {
    match outcome {
        MonitorOutcome::Success(msg) => EmulatorOutcome::Passed(msg),
        MonitorOutcome::Error(msg) => {
            // Heuristic: if the process crashed (non-zero exit with crash signature)
            if let Some(code) = exit_code {
                if code != 0 && (msg.contains("abort()") || msg.contains("Guru Meditation")) {
                    return EmulatorOutcome::Crashed(msg);
                }
            }
            EmulatorOutcome::Failed(msg)
        }
        MonitorOutcome::Timeout { expect_found } => EmulatorOutcome::TimedOut { expect_found },
        // Emulators never opt into ESP DTR/RTS recovery (no real hardware),
        // so this variant is unreachable from the emulator pipeline. Map it
        // to Failed defensively rather than panic if the future wires it up.
        MonitorOutcome::RecoverDownloadMode { signal } => EmulatorOutcome::Failed(format!(
            "unexpected ROM download-mode signal from emulator: {}",
            signal.diagnostic()
        )),
    }
}

pub(crate) fn qemu_session_dir(project_dir: &Path, env_name: &str) -> PathBuf {
    fbuild_paths::get_project_fbuild_dir(project_dir)
        .join("emulators")
        .join("qemu")
        .join(env_name)
        .join(uuid::Uuid::new_v4().to_string())
}

pub(crate) fn build_linux_macos_qemu_hint(err: &str) -> String {
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        format!(
            "{}. On Linux/macOS, ensure QEMU runtime deps are installed: libgcrypt, glib2, pixman, SDL2, and libslirp.",
            err
        )
    } else {
        err.to_string()
    }
}

pub(crate) async fn resolve_esp32_toolchain_gcc_path(
    project_dir: &Path,
    mcu_config: &fbuild_build::esp32::mcu_config::Esp32McuConfig,
) -> fbuild_core::Result<PathBuf> {
    let platform = fbuild_packages::library::Esp32Platform::new(project_dir);
    Package::ensure_installed(&platform).await?;

    let is_riscv = mcu_config.is_riscv();
    let prefix = mcu_config.toolchain_prefix();
    let toolchain_name = if is_riscv {
        "toolchain-riscv32-esp"
    } else {
        "toolchain-xtensa-esp-elf"
    };

    let toolchain = match platform.get_toolchain_metadata_url(is_riscv) {
        Ok(metadata_url) => {
            let cache = fbuild_packages::Cache::new(project_dir);
            let cache_dir = cache.toolchains_dir().join(toolchain_name);
            match fbuild_packages::toolchain::esp32_metadata::resolve_toolchain_url(
                &metadata_url,
                toolchain_name,
                &cache_dir,
            )
            .await
            {
                Ok(resolved) => fbuild_packages::toolchain::Esp32Toolchain::from_resolved(
                    project_dir,
                    &resolved.url,
                    resolved.sha256.as_deref(),
                    is_riscv,
                    &prefix,
                ),
                Err(_) => {
                    fbuild_packages::toolchain::Esp32Toolchain::new(project_dir, is_riscv, &prefix)
                }
            }
        }
        Err(_) => fbuild_packages::toolchain::Esp32Toolchain::new(project_dir, is_riscv, &prefix),
    };

    let _ = Package::ensure_installed(&toolchain).await?;
    Ok(toolchain.get_gcc_path())
}

#[cfg(windows)]
fn apply_windows_process_flags(cmd: &mut tokio::process::Command, exe_path: &Path) {
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    cmd.creation_flags(CREATE_NO_WINDOW);

    let current_path = std::env::var("PATH").unwrap_or_default();
    if let Ok(path_env) =
        fbuild_packages::toolchain::build_windows_qemu_path_env(exe_path, &current_path)
    {
        cmd.env("PATH", path_env);
    }
}

#[cfg(not(windows))]
fn apply_windows_process_flags(_cmd: &mut tokio::process::Command, _exe_path: &Path) {}

pub(crate) async fn spawn_line_reader(
    stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    is_stderr: bool,
    tx: UnboundedSender<ProcessEvent>,
) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let _ = tx.send(ProcessEvent::Line(ProcessLine { is_stderr, line }));
    }
    let _ = tx.send(ProcessEvent::StreamClosed);
}

pub(crate) async fn run_qemu_process(
    qemu_path: &Path,
    args: &[String],
    options: RunQemuOptions<'_>,
) -> fbuild_core::Result<QemuRunResult> {
    // allow-direct-spawn: tokio streaming QEMU emulator; blocking NativeProcess unsuitable.
    let mut cmd = tokio::process::Command::new(qemu_path);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_windows_process_flags(&mut cmd, qemu_path);

    let label = options.process_label;
    if options.verbose {
        tracing::info!("{}: {} {}", label, qemu_path.display(), args.join(" "));
    }

    // Route through containment (#32). QEMU is a long-running process
    // — a daemon hard-kill mid-emulation must not leave `qemu-system-*`
    // or its `conhost.exe` wrapper behind.
    let mut child =
        fbuild_core::containment::tokio_spawn::spawn_contained(&mut cmd).map_err(|e| {
            fbuild_core::FbuildError::DeployFailed(build_linux_macos_qemu_hint(&format!(
                "failed to launch {} at {}: {}",
                label,
                qemu_path.display(),
                e
            )))
        })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        fbuild_core::FbuildError::DeployFailed(format!("failed to capture {} stdout", label))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        fbuild_core::FbuildError::DeployFailed(format!("failed to capture {} stderr", label))
    })?;

    let (tx, mut rx) = unbounded::<ProcessEvent>();
    let stdout_task = tokio::spawn(spawn_line_reader(stdout, false, tx.clone()));
    let stderr_task = tokio::spawn(spawn_line_reader(stderr, true, tx));

    let mut monitor = MonitorState::new(
        options.timeout_secs,
        options.halt_on_error,
        options.halt_on_success,
        options.expect,
        options.show_timestamp,
        // Emulator path: no real DTR/RTS lines to drive, so the
        // auto-recovery from-ROM-download flag has nothing to do.
        false,
    );
    let mut crash_decoder =
        fbuild_serial::crash_decoder::CrashDecoder::new(options.elf_path, options.addr2line_path);
    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
    let mut synthetic_buf = String::new();
    let mut streams_open = 2usize;
    let mut child_exit: Option<std::process::ExitStatus> = None;
    let mut final_outcome: Option<MonitorOutcome> = None;

    loop {
        if monitor.timed_out() {
            final_outcome = Some(monitor.timeout_outcome());
            let _ = child.kill().await;
            break;
        }

        let recv_timeout = monitor
            .remaining()
            .unwrap_or(std::time::Duration::from_secs(1));

        tokio::select! {
            status = child.wait(), if child_exit.is_none() => {
                child_exit = Some(status.map_err(|e| {
                    fbuild_core::FbuildError::DeployFailed(format!("{} wait failed: {}", label, e))
                })?);
                if streams_open == 0 {
                    break;
                }
            }
            maybe_event = tokio::time::timeout(recv_timeout, rx.recv()) => {
                match maybe_event {
                    Ok(Some(ProcessEvent::Line(line))) => {
                        let target = if line.is_stderr { &mut stderr_buf } else { &mut stdout_buf };
                        target.push_str(&line.line);
                        target.push('\n');

                        if let Some(outcome) = monitor.process_line(&line.line) {
                            final_outcome = Some(outcome);
                            let _ = child.kill().await;
                            break;
                        }

                        if let Some(decoded_lines) = crash_decoder.process_line(&line.line).await {
                            for decoded in decoded_lines {
                                synthetic_buf.push_str(&decoded);
                                synthetic_buf.push('\n');
                                if let Some(outcome) = monitor.process_line(&decoded) {
                                    final_outcome = Some(outcome);
                                    let _ = child.kill().await;
                                    break;
                                }
                            }
                            if final_outcome.is_some() {
                                break;
                            }
                        }
                    }
                    Ok(Some(ProcessEvent::StreamClosed)) => {
                        streams_open = streams_open.saturating_sub(1);
                        if streams_open == 0 && child_exit.is_some() {
                            break;
                        }
                    }
                    Ok(None) => {
                        if child_exit.is_some() {
                            break;
                        }
                    }
                    Err(_) => {
                        final_outcome = Some(monitor.timeout_outcome());
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
        }
    }

    if child_exit.is_none() {
        // FastLED/fbuild#808 (HIGH): cap the post-kill reap. If the OS
        // kill is processed but the exit-status reaper hangs (rare but
        // observed on Windows with driver-resident children), let the
        // containment group reap the child on daemon exit instead of
        // blocking this handler forever.
        const CHILD_WAIT_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        match tokio::time::timeout(CHILD_WAIT_REAP_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => child_exit = Some(status),
            Ok(Err(e)) => {
                return Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "{} wait failed: {}",
                    label, e
                )));
            }
            Err(_) => {
                tracing::warn!(
                    "{} child.wait() exceeded {}s after kill; containment group will reap",
                    label,
                    CHILD_WAIT_REAP_TIMEOUT.as_secs()
                );
            }
        }
    }

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if !synthetic_buf.is_empty() {
        stdout_buf.push_str(&synthetic_buf);
    }

    let outcome = if let Some(outcome) = final_outcome {
        outcome
    } else if let Some(status) = child_exit {
        if status.success() {
            if options.expect.is_some() && !monitor.expect_found() {
                MonitorOutcome::Error(format!(
                    "{} exited before the expected pattern was found",
                    label
                ))
            } else {
                MonitorOutcome::Success(format!("{} exited normally", label))
            }
        } else {
            MonitorOutcome::Error(format!(
                "{} exited with code {}",
                label,
                status.code().unwrap_or(-1)
            ))
        }
    } else {
        MonitorOutcome::Error(format!("{} exited unexpectedly", label))
    };

    Ok(QemuRunResult {
        outcome,
        stdout: stdout_buf,
        stderr: stderr_buf,
        exit_code: child_exit.and_then(|s| s.code()),
    })
}

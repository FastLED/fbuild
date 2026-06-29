//! Headless AVR8js runner: spawns Node.js with the bundled `headless.mjs`
//! shim and streams the simulated UART output through the same monitor
//! state machine the real serial pipeline uses.

use super::avr8js_npm::{avr8js_cache_is_intact, REFRESH_EMU_CACHE_ENV};
use super::shared::{spawn_line_reader, ProcessEvent};
use crate::handlers::operations::{MonitorOutcome, MonitorState};
use fbuild_core::channel::unbounded;
use std::path::Path;
use std::process::Stdio;

pub(crate) const AVR8JS_HEADLESS_MJS: &str = include_str!("../../../web/avr8js/headless.mjs");

pub(crate) struct Avr8jsRunResult {
    pub outcome: MonitorOutcome,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

pub(crate) struct RunAvr8jsHeadlessOptions<'a> {
    pub timeout_secs: Option<f64>,
    pub halt_on_error: Option<&'a str>,
    pub halt_on_success: Option<&'a str>,
    pub expect: Option<&'a str>,
    pub show_timestamp: bool,
    pub verbose: bool,
}

pub(crate) async fn run_avr8js_headless(
    node_path: &Path,
    script_path: &Path,
    hex_path: &Path,
    f_cpu_hz: u32,
    avr8js_cache_dir: &Path,
    options: RunAvr8jsHeadlessOptions<'_>,
) -> fbuild_core::Result<Avr8jsRunResult> {
    // Fail-fast: re-verify the cache is intact before we spawn Node. This
    // guards against race conditions (cache wiped between `ensure_avr8js_npm`
    // and this call) and makes the error actionable instead of a cryptic
    // Node `ERR_MODULE_NOT_FOUND` stack trace (issue #86).
    if !avr8js_cache_is_intact(avr8js_cache_dir) {
        tracing::error!(
            "avr8js cache not populated at {}; aborting (expected \
             node_modules/avr8js/package.json). Set {}=1 to force reinstall.",
            avr8js_cache_dir.display(),
            REFRESH_EMU_CACHE_ENV
        );
        return Err(fbuild_core::FbuildError::DeployFailed(format!(
            "avr8js cache not populated at {} (missing \
             node_modules/avr8js/package.json). \
             Set {}=1 to force reinstall.",
            avr8js_cache_dir.display(),
            REFRESH_EMU_CACHE_ENV
        )));
    }

    // allow-direct-spawn: tokio streaming emulator; blocking NativeProcess unsuitable.
    let mut cmd = tokio::process::Command::new(node_path);
    cmd.arg(script_path)
        .arg("--hex")
        .arg(hex_path)
        .arg("--f-cpu")
        .arg(f_cpu_hz.to_string())
        .env("NODE_PATH", avr8js_cache_dir.join("node_modules"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    if options.verbose {
        tracing::info!(
            "avr8js headless: {} {} --hex {} --f-cpu {}",
            node_path.display(),
            script_path.display(),
            hex_path.display(),
            f_cpu_hz
        );
    }

    // Route through containment (#32) so a daemon crash mid-emulation
    // takes node.exe and any helper processes it spawned with it.
    let mut child =
        fbuild_core::containment::tokio_spawn::spawn_contained(&mut cmd).map_err(|e| {
            fbuild_core::FbuildError::DeployFailed(format!(
                "failed to launch Node.js for avr8js: {}",
                e
            ))
        })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        fbuild_core::FbuildError::DeployFailed("failed to capture avr8js stdout".to_string())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        fbuild_core::FbuildError::DeployFailed("failed to capture avr8js stderr".to_string())
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
        // No ESP hardware behind avr8js — auto-recovery has nothing to do.
        false,
    );
    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
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
                    fbuild_core::FbuildError::DeployFailed(format!("avr8js wait failed: {}", e))
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
        // FastLED/fbuild#808 (HIGH): cap the post-kill reap so a
        // driver-resident Node child stuck mid-exit cannot wedge the
        // handler. The containment group will eventually reap it.
        const AVR8JS_WAIT_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        match tokio::time::timeout(AVR8JS_WAIT_REAP_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => child_exit = Some(status),
            Ok(Err(e)) => {
                return Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "avr8js wait failed: {}",
                    e
                )));
            }
            Err(_) => {
                tracing::warn!(
                    "avr8js child.wait() exceeded {}s after kill; containment group will reap",
                    AVR8JS_WAIT_REAP_TIMEOUT.as_secs()
                );
            }
        }
    }

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    let outcome = if let Some(outcome) = final_outcome {
        outcome
    } else if let Some(status) = child_exit {
        if status.success() {
            if options.expect.is_some() && !monitor.expect_found() {
                MonitorOutcome::Error(
                    "avr8js exited before the expected pattern was found".to_string(),
                )
            } else {
                MonitorOutcome::Success("avr8js exited normally".to_string())
            }
        } else {
            MonitorOutcome::Error(format!(
                "avr8js exited with code {}",
                status.code().unwrap_or(-1)
            ))
        }
    } else {
        MonitorOutcome::Error("avr8js exited unexpectedly".to_string())
    };

    Ok(Avr8jsRunResult {
        outcome,
        stdout: stdout_buf,
        stderr: stderr_buf,
        exit_code: child_exit.and_then(|s| s.code()),
    })
}

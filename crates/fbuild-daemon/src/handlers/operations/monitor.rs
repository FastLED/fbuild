//! `POST /api/monitor` and the shared monitor-loop state machine used
//! by both the standalone monitor handler and the post-deploy monitor
//! attached to `/api/deploy`.

use crate::context::DaemonContext;
use crate::models::{MonitorRequest, OperationResponse};
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use std::sync::Arc;

/// Outcome of a post-deploy monitor session.
#[derive(Debug)]
pub(crate) enum MonitorOutcome {
    /// halt-on-success pattern matched
    Success(String),
    /// halt-on-error pattern matched
    Error(String),
    /// Timeout reached
    Timeout { expect_found: bool },
    /// ESP ROM download-mode was detected AND the caller opted in to
    /// auto-recovery via `MonitorRequest.auto_recover_from_download_mode`.
    /// The HTTP handler issues an `esp_hard_reset` against the same port,
    /// then folds the recovery outcome back into Success/Error.
    /// See FastLED/fbuild#532.
    RecoverDownloadMode {
        signal: fbuild_serial::boot_mode::BootModeSignal,
    },
}

pub(crate) struct MonitorState {
    halt_error_re: Option<regex::Regex>,
    halt_success_re: Option<regex::Regex>,
    expect_re: Option<regex::Regex>,
    start: std::time::Instant,
    timeout_dur: Option<std::time::Duration>,
    expect_found: bool,
    show_timestamp: bool,
    auto_recover_from_download_mode: bool,
}

impl MonitorState {
    pub(crate) fn new(
        timeout_secs: Option<f64>,
        halt_on_error: Option<&str>,
        halt_on_success: Option<&str>,
        expect: Option<&str>,
        show_timestamp: bool,
        auto_recover_from_download_mode: bool,
    ) -> Self {
        let halt_error_re = halt_on_error.and_then(|p| {
            regex::RegexBuilder::new(p)
                .case_insensitive(true)
                .build()
                .ok()
        });
        let halt_success_re = halt_on_success.and_then(|p| {
            regex::RegexBuilder::new(p)
                .case_insensitive(true)
                .build()
                .ok()
        });
        let expect_re = expect.and_then(|p| {
            regex::RegexBuilder::new(p)
                .case_insensitive(true)
                .build()
                .ok()
        });
        Self {
            halt_error_re,
            halt_success_re,
            expect_re,
            start: std::time::Instant::now(),
            timeout_dur: timeout_secs.map(std::time::Duration::from_secs_f64),
            expect_found: false,
            show_timestamp,
            auto_recover_from_download_mode,
        }
    }

    pub(crate) fn timed_out(&self) -> bool {
        self.timeout_dur
            .is_some_and(|dur| self.start.elapsed() >= dur)
    }

    pub(crate) fn remaining(&self) -> Option<std::time::Duration> {
        self.timeout_dur
            .map(|dur| dur.saturating_sub(self.start.elapsed()))
    }

    pub(crate) fn timeout_outcome(&self) -> MonitorOutcome {
        MonitorOutcome::Timeout {
            expect_found: self.expect_found,
        }
    }

    pub(crate) fn expect_found(&self) -> bool {
        self.expect_found
    }

    pub(crate) fn process_line(&mut self, line: &str) -> Option<MonitorOutcome> {
        if self.show_timestamp {
            let total_secs = self.start.elapsed().as_secs_f64();
            let minutes = (total_secs / 60.0) as u64;
            let seconds = total_secs % 60.0;
            tracing::info!("{:02}:{:05.2} {}", minutes, seconds, line);
        } else {
            tracing::info!("{}", line);
        }

        if let Some(ref re) = self.expect_re {
            if re.is_match(line) {
                self.expect_found = true;
            }
        }

        // A board stuck in the ESP ROM serial bootloader ("download mode")
        // never emits application output, so a monitor that keeps waiting for
        // the full timeout looks like a host-side deadlock and returns a bland
        // "completed (timeout)" success while the board is effectively bricked.
        // Halt immediately and surface the exact boot-mode problem so callers
        // (and the post-deploy monitor) fail fast with an actionable message.
        // See FastLED/fbuild#532.
        if let Some(signal) = fbuild_serial::boot_mode::detect_download_mode(line) {
            tracing::warn!("{}", signal.diagnostic());
            if self.auto_recover_from_download_mode {
                // Caller opted in; let the HTTP handler issue the DTR/RTS
                // hard-reset against the owned port and fold the recovery
                // outcome back into Success/Error.
                return Some(MonitorOutcome::RecoverDownloadMode { signal });
            }
            return Some(MonitorOutcome::Error(format!(
                "ESP ROM download-mode detected: {}",
                signal.diagnostic()
            )));
        }

        if let Some(ref re) = self.halt_error_re {
            if re.is_match(line) {
                return Some(MonitorOutcome::Error(format!(
                    "halt-on-error pattern matched: {}",
                    line
                )));
            }
        }

        if let Some(ref re) = self.halt_success_re {
            if re.is_match(line) {
                return Some(MonitorOutcome::Success(format!(
                    "halt-on-success pattern matched: {}",
                    line
                )));
            }
        }

        None
    }
}

/// Run a monitor loop reading lines from broadcast, checking halt conditions
/// using case-insensitive regex (matching Python's re.search behavior).
pub(crate) async fn run_monitor_loop(
    rx: &mut tokio::sync::broadcast::Receiver<String>,
    timeout_secs: Option<f64>,
    halt_on_error: Option<&str>,
    halt_on_success: Option<&str>,
    expect: Option<&str>,
    show_timestamp: bool,
    auto_recover_from_download_mode: bool,
) -> MonitorOutcome {
    let mut state = MonitorState::new(
        timeout_secs,
        halt_on_error,
        halt_on_success,
        expect,
        show_timestamp,
        auto_recover_from_download_mode,
    );
    loop {
        if state.timed_out() {
            return state.timeout_outcome();
        }

        let recv_timeout = state
            .remaining()
            .unwrap_or(std::time::Duration::from_secs(1));

        match tokio::time::timeout(recv_timeout, rx.recv()).await {
            Ok(Ok(line)) => {
                if let Some(outcome) = state.process_line(&line) {
                    return outcome;
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                tracing::warn!("monitor lagged, skipped {} messages", n);
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                return state.timeout_outcome();
            }
            Err(_) => {
                // Timeout on recv — check if overall timeout expired
                if state.timed_out() {
                    return state.timeout_outcome();
                }
                // No overall timeout: just keep waiting
            }
        }
    }
}

/// POST /api/monitor
pub async fn monitor(
    State(ctx): State<Arc<DaemonContext>>,
    Json(req): Json<MonitorRequest>,
) -> (StatusCode, Json<OperationResponse>) {
    let request_id = req
        .request_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let port = req.port.unwrap_or_else(|| "/dev/ttyUSB0".to_string());
    let baud_rate = req.baud_rate.unwrap_or(115200);

    if let Err(e) = ctx
        .serial_manager
        .open_port(&port, baud_rate, &request_id)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(OperationResponse::fail(
                request_id,
                format!("failed to open port: {}", e),
            )),
        );
    }

    // If halt conditions or timeout are set, run a monitor loop
    let has_conditions = req.halt_on_error.is_some()
        || req.halt_on_success.is_some()
        || req.expect.is_some()
        || req.timeout.is_some();

    if has_conditions {
        let mut rx = match ctx.serial_manager.attach_reader(&port, &request_id) {
            Some(rx) => rx,
            None => {
                return (
                    StatusCode::OK,
                    Json(OperationResponse::ok(
                        request_id,
                        format!(
                            "monitoring {} at {} baud (no broadcast channel)",
                            port, baud_rate
                        ),
                    )),
                );
            }
        };

        let raw_result = run_monitor_loop(
            &mut rx,
            req.timeout,
            req.halt_on_error.as_deref(),
            req.halt_on_success.as_deref(),
            req.expect.as_deref(),
            req.show_timestamp,
            req.auto_recover_from_download_mode,
        )
        .await;

        // Fold the ESP auto-recovery branch into Success/Error BEFORE we
        // detach the reader and close the port — `esp_hard_reset` borrows
        // the same `serial_handle` we'd be tearing down below. See
        // FastLED/fbuild#532.
        let result = match raw_result {
            MonitorOutcome::RecoverDownloadMode { signal } => {
                tracing::info!(
                    port,
                    "monitor: attempting ESP auto-recovery from ROM download mode"
                );
                match ctx.serial_manager.esp_hard_reset(&port, &request_id).await {
                    Ok(()) => MonitorOutcome::Success(format!(
                        "ESP auto-recovery succeeded after detecting {}: pulsed DTR/RTS \
                         hard-reset; chip should now boot from flash",
                        signal.diagnostic()
                    )),
                    Err(e) => MonitorOutcome::Error(format!(
                        "ESP auto-recovery failed after detecting {}: {}",
                        signal.diagnostic(),
                        e
                    )),
                }
            }
            other => other,
        };

        ctx.serial_manager.detach_reader(&port, &request_id);
        // Delay physical close briefly so close -> immediate reconnect
        // patterns remain logical and don't thrash the USB CDC handle.
        // Mirrors the WebSocket cleanup path. See FastLED/fbuild#592/#632.
        if !ctx.serial_manager.has_clients(&port) {
            ctx.serial_manager.close_port_after_grace_if_idle(
                &port,
                &request_id,
                std::time::Duration::from_secs(2),
            );
        }

        return match result {
            MonitorOutcome::Success(msg) => {
                (StatusCode::OK, Json(OperationResponse::ok(request_id, msg)))
            }
            MonitorOutcome::Error(msg) => (
                StatusCode::OK,
                Json(OperationResponse::fail(request_id, msg)),
            ),
            MonitorOutcome::Timeout { expect_found } => {
                if req.expect.is_some() && !expect_found {
                    (
                        StatusCode::OK,
                        Json(OperationResponse::fail(
                            request_id,
                            "monitor timed out (expected pattern not found)".to_string(),
                        )),
                    )
                } else {
                    (
                        StatusCode::OK,
                        Json(OperationResponse::ok(
                            request_id,
                            "monitor completed (timeout)".to_string(),
                        )),
                    )
                }
            }
            // Unreachable: the fold above folds `RecoverDownloadMode` into
            // Success or Error before this match runs. Defensive arm rather
            // than `unreachable!()` so a future caller that bypasses the fold
            // gets a clean error response instead of a daemon panic.
            MonitorOutcome::RecoverDownloadMode { signal } => (
                StatusCode::OK,
                Json(OperationResponse::fail(
                    request_id,
                    format!(
                        "internal: RecoverDownloadMode escaped the auto-recovery \
                         fold ({})",
                        signal.diagnostic()
                    ),
                )),
            ),
        };
    }

    (
        StatusCode::OK,
        Json(OperationResponse::ok(
            request_id,
            format!("monitoring {} at {} baud", port, baud_rate),
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MonitorState {
        MonitorState::new(
            Some(5.0),
            None,
            Some("READY"),
            Some("BOOT_OK"),
            false,
            false,
        )
    }

    fn state_with_auto_recover() -> MonitorState {
        MonitorState::new(Some(5.0), None, Some("READY"), Some("BOOT_OK"), false, true)
    }

    #[test]
    fn download_mode_line_halts_with_diagnostic() {
        let mut s = state();
        let outcome = s
            .process_line("rst:0x1 (POWERON),boot:0x23 DOWNLOAD(USB/UART0)")
            .expect("download-mode line must halt the monitor");
        match outcome {
            MonitorOutcome::Error(msg) => {
                assert!(
                    msg.contains("download-mode"),
                    "diagnostic should name the boot-mode problem, got: {msg}"
                );
            }
            other => panic!("expected Error outcome, got {other:?}"),
        }
    }

    #[test]
    fn download_mode_routes_to_recover_when_auto_recover_enabled() {
        let mut s = state_with_auto_recover();
        let outcome = s
            .process_line("waiting for download")
            .expect("download-mode line must halt the monitor");
        match outcome {
            MonitorOutcome::RecoverDownloadMode { signal } => {
                assert_eq!(
                    signal,
                    fbuild_serial::boot_mode::BootModeSignal::WaitingForDownload,
                    "the WaitingForDownload signal should propagate so the HTTP \
                     handler can attribute the recovery in its response"
                );
            }
            other => panic!("expected RecoverDownloadMode outcome, got {other:?}"),
        }
    }

    #[test]
    fn auto_recover_flag_does_not_affect_normal_halt_paths() {
        let mut s = state_with_auto_recover();
        // Application output that is NOT a download-mode signal must still
        // flow through the regular halt-on-success / halt-on-error / expect
        // pipeline; the auto-recover flag only changes the download-mode
        // branch. In `state_with_auto_recover` the halt-on-success regex is
        // "READY" and the expect regex is "BOOT_OK" (matches `state()`).
        assert!(s.process_line("Hello from app_main").is_none());
        assert!(matches!(
            s.process_line("system READY now"),
            Some(MonitorOutcome::Success(_))
        ));
    }

    #[test]
    fn waiting_for_download_halts() {
        let mut s = state();
        assert!(
            matches!(
                s.process_line("waiting for download"),
                Some(MonitorOutcome::Error(_))
            ),
            "`waiting for download` must surface as an error, not a silent wait"
        );
    }

    #[test]
    fn normal_app_output_does_not_halt() {
        let mut s = state();
        assert!(s.process_line("Hello from app_main").is_none());
        assert!(s.process_line("boot:0x13 (SPI_FAST_FLASH_BOOT)").is_none());
    }

    #[test]
    fn halt_on_success_and_expect_still_work() {
        let mut s = state();
        // expect is observed without halting.
        assert!(s.process_line("BOOT_OK reached").is_none());
        assert!(s.expect_found());
        // halt-on-success still terminates the loop.
        match s.process_line("system READY now") {
            Some(MonitorOutcome::Success(msg)) => assert!(msg.contains("READY")),
            other => panic!("expected Success outcome, got {other:?}"),
        }
    }
}

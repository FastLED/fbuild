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
}

pub(crate) struct MonitorState {
    halt_error_re: Option<regex::Regex>,
    halt_success_re: Option<regex::Regex>,
    expect_re: Option<regex::Regex>,
    start: std::time::Instant,
    timeout_dur: Option<std::time::Duration>,
    expect_found: bool,
    show_timestamp: bool,
}

impl MonitorState {
    pub(crate) fn new(
        timeout_secs: Option<f64>,
        halt_on_error: Option<&str>,
        halt_on_success: Option<&str>,
        expect: Option<&str>,
        show_timestamp: bool,
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
) -> MonitorOutcome {
    let mut state = MonitorState::new(
        timeout_secs,
        halt_on_error,
        halt_on_success,
        expect,
        show_timestamp,
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

        let result = run_monitor_loop(
            &mut rx,
            req.timeout,
            req.halt_on_error.as_deref(),
            req.halt_on_success.as_deref(),
            req.expect.as_deref(),
            req.show_timestamp,
        )
        .await;

        ctx.serial_manager.detach_reader(&port, &request_id);
        // Release the OS serial handle once no clients remain so a follow-up
        // pyserial/esptool open of the same port succeeds without requiring
        // `fbuild daemon stop`. Mirrors the WebSocket cleanup path.
        // See FastLED/fbuild#531.
        if !ctx.serial_manager.has_clients(&port) {
            if let Err(e) = ctx.serial_manager.close_port(&port, &request_id).await {
                tracing::warn!(port, "failed to close port after monitor exit: {}", e);
            }
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

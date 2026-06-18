//! WebSocket handlers for serial monitor, status streaming, and log streaming.

use base64::Engine;

use crate::context::DaemonContext;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use fbuild_serial::{SerialClientMessage, SerialServerMessage, SerialStreamEvent};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// /ws/serial-monitor — existing serial monitor WebSocket
// ---------------------------------------------------------------------------

/// GET /ws/serial-monitor — upgrade to WebSocket for serial monitor.
pub async fn ws_serial_monitor(
    ws: WebSocketUpgrade,
    State(ctx): State<Arc<DaemonContext>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_serial_ws(socket, ctx))
}

/// RAII guard that increments `pending_serial_attaches` on construction and
/// decrements on drop. Prevents the daemon's self-eviction loop from killing
/// the daemon while a WebSocket client is mid-attach (e.g. waiting for
/// `open_port` to complete its USB re-enumeration retries).
struct PendingAttachGuard {
    ctx: Arc<DaemonContext>,
}

impl PendingAttachGuard {
    fn new(ctx: Arc<DaemonContext>) -> Self {
        ctx.pending_serial_attaches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { ctx }
    }
}

impl Drop for PendingAttachGuard {
    fn drop(&mut self) {
        self.ctx
            .pending_serial_attaches
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Unwinds any serial-session state that the ws_serial_monitor handler
/// left on the shared manager: detach reader, release writer, and close
/// the port if there are no remaining clients. Idempotent — safe to call
/// on a partially-set-up session. See FastLED/fbuild#51.
async fn cleanup_ws_serial_session(
    ctx: &Arc<DaemonContext>,
    port: &str,
    client_id: &str,
    writer_acquired: bool,
) {
    ctx.serial_manager.detach_reader(port, client_id);
    if writer_acquired {
        ctx.serial_manager.release_writer(port, client_id);
    }
    if !ctx.serial_manager.has_clients(port) {
        ctx.serial_manager
            .close_port_after_grace_if_idle(port, client_id, Duration::from_secs(2));
    }
}

async fn handle_serial_ws(mut socket: WebSocket, ctx: Arc<DaemonContext>) {
    // Mark this attach as pending so the self-eviction loop won't shut the
    // daemon down while we're waiting for `open_port` to finish (USB
    // re-enumeration on Windows can take 10+ seconds).
    let _attach_guard = PendingAttachGuard::new(ctx.clone());

    // Wait for the attach message
    let (client_id, port, baud_rate, pre_acquire_writer) = match socket.recv().await {
        Some(Ok(Message::Text(text))) => {
            match serde_json::from_str::<SerialClientMessage>(&text) {
                Ok(SerialClientMessage::Attach {
                    client_id,
                    port,
                    baud_rate,
                    open_if_needed,
                    pre_acquire_writer,
                }) => {
                    // Open port if needed
                    if open_if_needed {
                        if let Err(e) = ctx
                            .serial_manager
                            .open_port(&port, baud_rate, &client_id)
                            .await
                        {
                            let err_msg = SerialServerMessage::Error {
                                message: format!("failed to open port: {}", e),
                            };
                            let _ = socket
                                .send(Message::Text(serde_json::to_string(&err_msg).unwrap()))
                                .await;
                            return;
                        }
                    }
                    (client_id, port, baud_rate, pre_acquire_writer)
                }
                Ok(_) => {
                    let err_msg = SerialServerMessage::Error {
                        message: "expected attach message first".to_string(),
                    };
                    let _ = socket
                        .send(Message::Text(serde_json::to_string(&err_msg).unwrap()))
                        .await;
                    return;
                }
                Err(e) => {
                    let err_msg = SerialServerMessage::Error {
                        message: format!("invalid message: {}", e),
                    };
                    let _ = socket
                        .send(Message::Text(serde_json::to_string(&err_msg).unwrap()))
                        .await;
                    return;
                }
            }
        }
        _ => return,
    };

    // From this point on, `open_port` has created state on the shared
    // manager (session + broadcaster) that MUST be torn down on every exit
    // path — otherwise `has_clients()` keeps returning true and the
    // daemon's self-eviction loop never fires. See FastLED/fbuild#51 for
    // the leak that kept fbuild-daemon resident after autoresearch ended.

    // Pre-acquire writer if requested
    let writer_acquired = if pre_acquire_writer {
        ctx.serial_manager
            .acquire_writer(&port, &client_id)
            .await
            .is_ok()
    } else {
        false
    };

    // Attach reader
    let mut rx = match ctx.serial_manager.attach_reader(&port, &client_id) {
        Some(rx) => rx,
        None => {
            let err_msg = SerialServerMessage::Error {
                message: format!("port {} not open", port),
            };
            let _ = socket
                .send(Message::Text(serde_json::to_string(&err_msg).unwrap()))
                .await;
            cleanup_ws_serial_session(&ctx, &port, &client_id, writer_acquired).await;
            return;
        }
    };

    // Send attached confirmation
    let attached = SerialServerMessage::Attached {
        success: true,
        message: format!("attached to {} at {} baud", port, baud_rate),
        writer_pre_acquired: writer_acquired,
    };
    if socket
        .send(Message::Text(serde_json::to_string(&attached).unwrap()))
        .await
        .is_err()
    {
        cleanup_ws_serial_session(&ctx, &port, &client_id, writer_acquired).await;
        return;
    }

    let mut line_index: u64 = 0;

    loop {
        tokio::select! {
            // Forward serial output to WebSocket
            result = rx.recv() => {
                match result {
                    Ok(SerialStreamEvent::Data(line)) => {
                        line_index += 1;
                        // A streaming serial monitor is "active" — bump
                        // last_activity on every line so the 12-hour
                        // IDLE_TIMEOUT doesn't kill a long unattended
                        // autoresearch run. Self-eviction is already
                        // handled separately by `pending_serial_attaches`
                        // + open serial session count, but `idle_duration()`
                        // is independent and would otherwise tick toward
                        // the 12h fallback. See ISSUES.md outstanding
                        // "self-eviction grace period during attach"
                        // (Issue A follow-up).
                        ctx.touch_activity();

                        // Process through crash decoder
                        let mut lines = vec![line.clone()];
                        if let Some(decoded) = ctx.serial_manager.process_crash_line(&port, &line) {
                            lines.extend(decoded);
                        }

                        let data_msg = SerialServerMessage::Data {
                            lines,
                            current_index: line_index,
                        };
                        if socket.send(Message::Text(serde_json::to_string(&data_msg).unwrap())).await.is_err() {
                            break;
                        }
                    }
                    Ok(SerialStreamEvent::PortDisconnected { port, reason, message }) => {
                        let msg = SerialServerMessage::PortDisconnected {
                            port,
                            reason,
                            message,
                        };
                        let _ = socket.send(Message::Text(serde_json::to_string(&msg).unwrap())).await;
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(client_id, port, n, "reader lagged, skipping lines");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            // Handle incoming WebSocket messages
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<SerialClientMessage>(&text) {
                            Ok(SerialClientMessage::Write { data }) => {
                                // Inbound writes also count as activity —
                                // resets idle timer so client-driven
                                // sessions (e.g. JSON-RPC over serial)
                                // keep the daemon hot. See Issue A
                                // follow-up in ISSUES.md.
                                ctx.touch_activity();
                                let decoded = match base64::engine::general_purpose::STANDARD.decode(&data) {
                                    Ok(d) => d,
                                    Err(e) => {
                                        let err_msg = SerialServerMessage::Error {
                                            message: format!("base64 decode error: {}", e),
                                        };
                                        let _ = socket.send(Message::Text(serde_json::to_string(&err_msg).unwrap())).await;
                                        continue;
                                    }
                                };
                                match ctx.serial_manager.write_to_port(&port, &decoded, &client_id).await {
                                    Ok(n) => {
                                        let ack = SerialServerMessage::WriteAck {
                                            success: true,
                                            bytes_written: n,
                                            message: None,
                                        };
                                        let _ = socket.send(Message::Text(serde_json::to_string(&ack).unwrap())).await;
                                    }
                                    Err(e) => {
                                        let ack = SerialServerMessage::WriteAck {
                                            success: false,
                                            bytes_written: 0,
                                            message: Some(format!("write error: {}", e)),
                                        };
                                        let _ = socket.send(Message::Text(serde_json::to_string(&ack).unwrap())).await;
                                        tracing::warn!(client_id, port, "write error: {}", e);
                                    }
                                }
                            }
                            Ok(SerialClientMessage::Detach) => {
                                break;
                            }
                            Ok(SerialClientMessage::ClearBuffer) => {
                                // FastLED/fbuild#605 — drop every line the
                                // client's broadcast receiver has buffered
                                // but not yet observed. Mirrors pyserial's
                                // `Serial.reset_input_buffer()` semantic
                                // (modulo bytes vs lines).
                                let mut drained: usize = 0;
                                while rx.try_recv().is_ok() {
                                    drained += 1;
                                }
                                tracing::debug!(
                                    client_id,
                                    port,
                                    drained,
                                    "clear_buffer drained pending lines"
                                );
                            }
                            Ok(SerialClientMessage::GetInWaiting) => {
                                // FastLED/fbuild#605 — answer with the
                                // current per-client broadcast queue depth
                                // (lines buffered but not yet observed).
                                // Distinct from pyserial's `in_waiting`
                                // (bytes) — see the issue for the rationale.
                                let count = rx.len();
                                let reply = SerialServerMessage::InWaiting { count };
                                if socket
                                    .send(Message::Text(
                                        serde_json::to_string(&reply).unwrap(),
                                    ))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!(client_id, "invalid ws message: {}", e);
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    // Cleanup: detach reader, release writer, and close the port if we
    // were the last client. Without the close, the daemon's background
    // reader keeps the OS file handle open, blocking other tools (e.g.
    // `pyserial.Serial(...)` from the same Python process) with
    // "Access is denied" until the daemon itself shuts down.
    cleanup_ws_serial_session(&ctx, &port, &client_id, writer_acquired).await;
}

// ---------------------------------------------------------------------------
// /ws/status — real-time daemon status updates
// ---------------------------------------------------------------------------

/// GET /ws/status — upgrade to WebSocket for real-time status updates.
///
/// On connect the server sends the current status snapshot. Afterwards the
/// client receives broadcasts whenever daemon state changes (build progress,
/// deploy, idle transitions, etc.).
///
/// Client → Server messages:
///   `{"type":"ping"}` → server replies `{"type":"pong","timestamp":…}`
///   `{"type":"get_status"}` → server replies with current status snapshot
///
/// Server → Client messages:
///   `{"type":"status","state":"building","message":"…","operation_in_progress":true,…}`
pub async fn ws_status(
    ws: WebSocketUpgrade,
    State(ctx): State<Arc<DaemonContext>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_status_ws(socket, ctx))
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Build a JSON status snapshot from the current daemon context.
fn build_status_snapshot(ctx: &DaemonContext) -> String {
    ctx.status_snapshot_json()
}

async fn handle_status_ws(mut socket: WebSocket, ctx: Arc<DaemonContext>) {
    tracing::info!("Status WebSocket connected");

    // Send initial status snapshot
    let initial = build_status_snapshot(&ctx);
    if socket.send(Message::Text(initial)).await.is_err() {
        return;
    }

    // Subscribe to status broadcast channel
    let mut rx = ctx.broadcast_hub.status_tx.subscribe();

    loop {
        tokio::select! {
            // Forward broadcast status updates to this client
            result = rx.recv() => {
                match result {
                    Ok(msg) => {
                        if socket.send(Message::Text(msg)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(n, "status ws client lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            // Handle incoming client messages (ping, get_status)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&text) {
                            match obj.get("type").and_then(|t| t.as_str()) {
                                Some("ping") => {
                                    let pong = serde_json::json!({"type": "pong", "timestamp": now_unix()}).to_string();
                                    let _ = socket.send(Message::Text(pong)).await;
                                }
                                Some("get_status") => {
                                    let snap = build_status_snapshot(&ctx);
                                    let _ = socket.send(Message::Text(snap)).await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    tracing::info!("Status WebSocket disconnected");
}

// ---------------------------------------------------------------------------
// /ws/logs — live daemon log streaming
// ---------------------------------------------------------------------------

/// GET /ws/logs — upgrade to WebSocket for live daemon log entries.
///
/// Client → Server: `{"type":"ping"}` only.
/// Server → Client: `{"type":"log","level":"INFO","message":"…","timestamp":…,"module":…}`
pub async fn ws_logs(
    ws: WebSocketUpgrade,
    State(ctx): State<Arc<DaemonContext>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_logs_ws(socket, ctx))
}

async fn handle_logs_ws(mut socket: WebSocket, ctx: Arc<DaemonContext>) {
    tracing::info!("Logs WebSocket connected");

    // Send welcome message
    let welcome = serde_json::json!({
        "type": "log",
        "level": "INFO",
        "message": "Connected to daemon log stream",
        "timestamp": now_unix(),
        "module": "websockets",
    })
    .to_string();
    if socket.send(Message::Text(welcome)).await.is_err() {
        return;
    }

    // Subscribe to log broadcast channel
    let mut rx = ctx.broadcast_hub.log_tx.subscribe();

    loop {
        tokio::select! {
            // Forward broadcast log entries to this client
            result = rx.recv() => {
                match result {
                    Ok(msg) => {
                        if socket.send(Message::Text(msg)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(n, "logs ws client lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            // Handle incoming client messages (ping only)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&text) {
                            if obj.get("type").and_then(|t| t.as_str()) == Some("ping") {
                                let pong = serde_json::json!({"type": "pong", "timestamp": now_unix()}).to_string();
                                let _ = socket.send(Message::Text(pong)).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    tracing::info!("Logs WebSocket disconnected");
}

// ---------------------------------------------------------------------------
// /ws/monitor/:session_id — serial monitor session by ID
// ---------------------------------------------------------------------------

/// GET /ws/monitor/:session_id — upgrade to WebSocket for a named monitor session.
///
/// A simpler monitor endpoint identified by `session_id`. Clients receive
/// serial data pushed by the connection manager and can write data back.
///
/// Client → Server: `{"type":"write","data":"…"}`, `{"type":"ping"}`
/// Server → Client: `{"type":"monitor_data","session_id":"…","data":"…","timestamp":…}`
pub async fn ws_monitor_session(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(ctx): State<Arc<DaemonContext>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_monitor_session_ws(socket, session_id, ctx))
}

async fn handle_monitor_session_ws(
    mut socket: WebSocket,
    session_id: String,
    _ctx: Arc<DaemonContext>,
) {
    tracing::info!(session_id, "Monitor session WebSocket connected");

    // Send welcome message
    let welcome = serde_json::json!({
        "type": "monitor_data",
        "session_id": &session_id,
        "data": format!("Connected to monitor session: {}\n", session_id),
        "timestamp": now_unix(),
    })
    .to_string();
    if socket.send(Message::Text(welcome)).await.is_err() {
        return;
    }

    // Keep connection alive and handle client messages
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&text) {
                    match obj.get("type").and_then(|t| t.as_str()) {
                        Some("ping") => {
                            let pong = serde_json::json!({"type": "pong", "timestamp": now_unix()})
                                .to_string();
                            let _ = socket.send(Message::Text(pong)).await;
                        }
                        Some("write") => {
                            // Acknowledge write (actual serial routing is done via
                            // /ws/serial-monitor which has full attach/detach protocol)
                            let ack = serde_json::json!({"type": "ack", "timestamp": now_unix()})
                                .to_string();
                            let _ = socket.send(Message::Text(ack)).await;
                        }
                        _ => {}
                    }
                } else {
                    let err = serde_json::json!({
                        "type": "error",
                        "error": "Invalid JSON",
                        "detail": "Could not parse message",
                    })
                    .to_string();
                    let _ = socket.send(Message::Text(err)).await;
                }
            }
            Some(Ok(Message::Close(_))) | None => break,
            _ => {}
        }
    }

    tracing::info!(session_id, "Monitor session WebSocket disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_status_snapshot_produces_valid_json() {
        let (tx, _rx) = tokio::sync::watch::channel(false);
        let ctx = DaemonContext::new(8765, tx, "test".to_string());
        let snap = build_status_snapshot(&ctx);
        let v: serde_json::Value = serde_json::from_str(&snap).unwrap();
        assert_eq!(v["type"], "status");
        assert_eq!(v["state"], "idle");
        assert!(!v["operation_in_progress"].as_bool().unwrap());
    }

    #[test]
    fn now_unix_returns_reasonable_value() {
        let ts = now_unix();
        // After 2020-01-01
        assert!(ts > 1_577_836_800.0);
    }
}

//! REMOTE: JSON-RPC response helpers and shared async read/write loops
//! used by both `SerialMonitor` and `AsyncSerialMonitor`.

use base64::Engine;
use futures::{SinkExt, StreamExt};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio_tungstenite::tungstenite;

use crate::messages::{ClientMessage, ServerMessage, WsSink, WsSource};

pub(crate) fn extract_remote_json_rpc_response(lines: &[String]) -> Option<String> {
    lines.iter().find_map(|line| {
        line.strip_prefix("REMOTE:")
            .map(|json_part| json_part.to_string())
    })
}

pub(crate) fn wait_for_remote_json_rpc_response<F>(timeout: f64, mut poll: F) -> Option<String>
where
    F: FnMut(f64) -> Vec<String>,
{
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);

    while std::time::Instant::now() < deadline {
        let remaining = (deadline - std::time::Instant::now()).as_secs_f64();
        let lines = poll(remaining);
        if let Some(json_part) = extract_remote_json_rpc_response(&lines) {
            return Some(json_part);
        }
    }

    None
}

/// Shared async read-batch loop used by `AsyncSerialMonitor::read_lines`
/// and `write_json_rpc`. Acquires the source mutex only for the duration
/// of each `.next()` call so that concurrent `write` / `__aexit__`
/// futures can still progress between iterations.
pub(crate) async fn read_lines_async(
    ws_read_slot: Arc<tokio::sync::Mutex<Option<WsSource>>>,
    pending_lines: Arc<tokio::sync::Mutex<VecDeque<String>>>,
    auto_reconnect: bool,
    timeout_secs: f64,
) -> Vec<String> {
    let mut lines = Vec::new();
    {
        let mut pending = pending_lines.lock().await;
        while let Some(line) = pending.pop_front() {
            lines.push(line);
        }
    }
    if !lines.is_empty() {
        return lines;
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_secs);

    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;

        let mut guard = ws_read_slot.lock().await;
        let Some(source) = guard.as_mut() else {
            // Session not entered (or already exited). Nothing to read.
            break;
        };

        let result = tokio::time::timeout(remaining, source.next()).await;
        // Drop the guard before continuing the loop so another task
        // can take the mutex (e.g. __aexit__).
        drop(guard);

        match result {
            Ok(Some(Ok(tungstenite::Message::Text(text)))) => {
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(ServerMessage::Data {
                        lines: data_lines, ..
                    }) => {
                        lines.extend(data_lines);
                        if !lines.is_empty() {
                            break;
                        }
                    }
                    Ok(ServerMessage::Preempted { .. }) => {
                        if auto_reconnect {
                            continue;
                        }
                        break;
                    }
                    Ok(ServerMessage::Reconnected { .. }) => continue,
                    Ok(ServerMessage::PortRenumbered { .. })
                    | Ok(ServerMessage::PortReattached { .. }) => continue,
                    Ok(ServerMessage::PortRebindFailed { .. }) => break,
                    Ok(ServerMessage::PortDisconnected { .. }) => break,
                    _ => continue,
                }
            }
            Ok(Some(Ok(tungstenite::Message::Close(_)))) | Ok(None) => break,
            Err(_) => break, // timeout
            _ => continue,
        }
    }

    lines
}

/// Shared async write path used by `AsyncSerialMonitor::write` and
/// `write_json_rpc`. Sends the already-base64-encoded payload, then
/// awaits a `write_ack` (with a 5-second bound matching the sync path).
/// Returns `true` if the ack reported any bytes written.
pub(crate) async fn write_async(
    ws_write_slot: Arc<tokio::sync::Mutex<Option<WsSink>>>,
    ws_read_slot: Arc<tokio::sync::Mutex<Option<WsSource>>>,
    pending_lines: Arc<tokio::sync::Mutex<VecDeque<String>>>,
    encoded: String,
) -> bool {
    let msg = serde_json::to_string(&ClientMessage::Write { data: encoded })
        .expect("fbuild-python: ClientMessage::Write serialization is infallible");

    {
        let mut guard = ws_write_slot.lock().await;
        let Some(sink) = guard.as_mut() else {
            return false;
        };
        if sink.send(tungstenite::Message::Text(msg)).await.is_err() {
            return false;
        }
    }

    // Wait for write_ack. Serial data can arrive before the ack on the
    // WebSocket; queue it for the next read rather than consuming it.
    let mut guard = ws_read_slot.lock().await;
    let Some(source) = guard.as_mut() else {
        return false;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let remaining = deadline - std::time::Instant::now();
        match tokio::time::timeout(remaining, source.next()).await {
            Ok(Some(Ok(tungstenite::Message::Text(text)))) => {
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(ServerMessage::WriteAck {
                        success,
                        bytes_written,
                        ..
                    }) => return success && bytes_written > 0,
                    Ok(ServerMessage::Data { lines, .. }) => {
                        pending_lines.lock().await.extend(lines);
                        continue;
                    }
                    Ok(ServerMessage::Preempted { .. })
                    | Ok(ServerMessage::Reconnected { .. })
                    | Ok(ServerMessage::PortRenumbered { .. })
                    | Ok(ServerMessage::PortReattached { .. })
                    | Ok(ServerMessage::Other) => continue,
                    Ok(ServerMessage::Error { .. })
                    | Ok(ServerMessage::PortDisconnected { .. })
                    | Ok(ServerMessage::PortRebindFailed { .. }) => return false,
                    _ => continue,
                }
            }
            Ok(Some(Ok(tungstenite::Message::Close(_)))) | Ok(None) => break,
            Err(_) => break,
            _ => continue,
        }
    }
    false
}

/// Async counterpart to `wait_for_remote_json_rpc_response`. Keeps
/// polling `read_lines_async` until the deadline expires, even if an
/// individual batch comes back empty — preserving the PR #57 fix.
pub(crate) async fn wait_for_remote_json_rpc_response_async(
    timeout_secs: f64,
    ws_read_slot: Arc<tokio::sync::Mutex<Option<WsSource>>>,
    pending_lines: Arc<tokio::sync::Mutex<VecDeque<String>>>,
    auto_reconnect: bool,
) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_secs);

    while std::time::Instant::now() < deadline {
        let remaining = (deadline - std::time::Instant::now()).as_secs_f64();
        if remaining <= 0.0 {
            break;
        }
        let lines = read_lines_async(
            ws_read_slot.clone(),
            pending_lines.clone(),
            auto_reconnect,
            remaining,
        )
        .await;
        if let Some(json_part) = extract_remote_json_rpc_response(&lines) {
            return Some(json_part);
        }
    }

    None
}

/// Helper: produce a base64-encoded write payload from raw bytes.
pub(crate) fn encode_payload(data: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(data.as_bytes())
}

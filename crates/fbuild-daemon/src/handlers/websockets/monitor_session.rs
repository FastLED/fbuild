use super::*;
use axum::extract::Path;

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

    // Keep connection alive and handle client messages.
    // FastLED/fbuild#808: idle clients used to keep this task pinned
    // forever; close the socket if no frame arrives within
    // `MONITOR_SESSION_IDLE_TIMEOUT`.
    const MONITOR_SESSION_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    loop {
        tokio::select! {
            recv = socket.recv() => match recv {
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
            },
            _ = tokio::time::sleep(MONITOR_SESSION_IDLE_TIMEOUT) => {
                tracing::info!(
                    session_id,
                    "Monitor session WebSocket idle for {}s; closing",
                    MONITOR_SESSION_IDLE_TIMEOUT.as_secs()
                );
                break;
            }
        }
    }

    tracing::info!(session_id, "Monitor session WebSocket disconnected");
}

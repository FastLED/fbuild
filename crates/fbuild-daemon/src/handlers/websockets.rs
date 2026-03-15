//! WebSocket handlers for serial monitor and status streaming.

use base64::Engine;

use crate::context::DaemonContext;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use fbuild_serial::{SerialClientMessage, SerialServerMessage};
use std::sync::Arc;

/// GET /ws/serial-monitor — upgrade to WebSocket for serial monitor.
pub async fn ws_serial_monitor(
    ws: WebSocketUpgrade,
    State(ctx): State<Arc<DaemonContext>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_serial_ws(socket, ctx))
}

async fn handle_serial_ws(mut socket: WebSocket, ctx: Arc<DaemonContext>) {
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
        return;
    }

    let mut line_index: u64 = 0;

    loop {
        tokio::select! {
            // Forward serial output to WebSocket
            result = rx.recv() => {
                match result {
                    Ok(line) => {
                        line_index += 1;

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

    // Cleanup
    ctx.serial_manager.detach_reader(&port, &client_id);
    if writer_acquired {
        ctx.serial_manager.release_writer(&port, &client_id);
    }
}

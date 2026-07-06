//! WebSocket handlers for serial monitor, status streaming, and log streaming.

use base64::Engine;

use crate::context::DaemonContext;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use fbuild_core::channel as mpsc;
use fbuild_serial::{SerialClientMessage, SerialServerMessage, SerialStreamEvent};
use futures::{SinkExt, StreamExt};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

/// Serialize a `SerialServerMessage` (or any `serde::Serialize` value)
/// to JSON, falling back to a hardcoded JSON error frame if serialization
/// somehow fails. Used on WebSocket error-reply paths where panicking
/// would tear the whole socket down instead of just dropping one frame.
///
/// `serde_json::to_string` only fails for values whose `Serialize` impl
/// returns an error or that contain non-finite floats inside a map key
/// — neither shape occurs in `SerialServerMessage`. The fallback exists
/// purely as a panic-free guarantee for the cold path; FastLED/fbuild#826
/// flagged the prior `.unwrap()` calls as a stability hazard.
fn serialize_or_fallback<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| r#"{"type":"error","message":"<internal serde failure>"}"#.to_string())
}

// ReaderControl -- inbound -> reader cross-task RPC for the small set
// of `SerialClientMessage`s that need read-only access to the reader-
// owned broadcast receiver (`ClearBuffer` and `GetInWaiting`).
//
// Pre-#756 these two RPCs were logged no-ops because the post-#749/#750
// reader/writer/inbound split moved `rx` exclusively into the reader
// task. Adding a control channel + oneshot reply lets inbound borrow
// the operation through the reader without exposing `rx` itself, so
// the original FastLED/fbuild#605 semantics are restored without
// regressing the throughput fix.
//
// Variants intentionally minimal -- one per RPC. New protocol RPCs
// that need read-only `rx` access get a new variant.
enum ReaderControl {
    /// Drain `rx` of any buffered events. Reply: number of events dropped.
    Drain { reply: oneshot::Sender<usize> },
    /// Report `rx.len()` (broadcast queue depth). Reply: count.
    GetDepth { reply: oneshot::Sender<usize> },
}

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
    id: u64,
}

impl PendingAttachGuard {
    fn new(ctx: Arc<DaemonContext>) -> Self {
        let id = ctx.begin_pending_serial_attach();
        Self { ctx, id }
    }

    fn set_target(&self, client_id: String, port: String) {
        self.ctx
            .update_pending_serial_attach(self.id, client_id, port);
    }
}

impl Drop for PendingAttachGuard {
    fn drop(&mut self) {
        self.ctx.end_pending_serial_attach(self.id);
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

/// Cap on how long the daemon will wait for a WebSocket client to send
/// its first frame after upgrade. A client that completes the WS upgrade
/// then sends nothing used to keep this handler suspended until the OS
/// TCP keepalive fired (potentially hours), consuming a tokio task slot
/// for every dead connection. See FastLED/fbuild#808.
const WS_ATTACH_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Cap on the WebSocket serial attach `open_port` await. HTTP monitor and
/// post-deploy monitor paths already bound this call at 30 s; the WebSocket
/// attach path needs the same ceiling so a wedged USB driver cannot leave a
/// pending serial attach counted forever. See FastLED/fbuild#977.
const WS_SERIAL_OPEN_PORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn format_timeout_for_error(timeout: Duration) -> String {
    let millis = timeout.as_millis();
    if millis > 0 && millis < 1_000 {
        format!("{millis}ms")
    } else {
        format!("{}s", timeout.as_secs())
    }
}

async fn await_ws_serial_open_port<F>(
    port: &str,
    open_future: F,
    timeout: Duration,
) -> Result<(), String>
where
    F: Future<Output = fbuild_core::Result<()>>,
{
    match tokio::time::timeout(timeout, open_future).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("failed to open port: {}", e)),
        Err(_) => Err(format!(
            "open_port({}) exceeded {}; serial driver may be wedged",
            port,
            format_timeout_for_error(timeout)
        )),
    }
}

async fn handle_serial_ws(mut socket: WebSocket, ctx: Arc<DaemonContext>) {
    // Mark this attach as pending so the self-eviction loop won't shut the
    // daemon down while we're waiting for `open_port` to finish (USB
    // re-enumeration on Windows can take 10+ seconds).
    let attach_guard = PendingAttachGuard::new(ctx.clone());

    // Wait for the attach message (FastLED/fbuild#808: bounded so an
    // idle client cannot tie up a tokio task slot forever).
    let first_frame = match tokio::time::timeout(WS_ATTACH_HANDSHAKE_TIMEOUT, socket.recv()).await {
        Ok(frame) => frame,
        Err(_) => {
            let err_msg = SerialServerMessage::Error {
                message: format!(
                    "attach handshake not received within {}s; closing connection",
                    WS_ATTACH_HANDSHAKE_TIMEOUT.as_secs()
                ),
            };
            let _ = socket
                .send(Message::Text(serialize_or_fallback(&err_msg)))
                .await;
            return;
        }
    };
    let (client_id, port, baud_rate, pre_acquire_writer, client_metadata) = match first_frame {
        Some(Ok(Message::Text(text))) => {
            match serde_json::from_str::<SerialClientMessage>(&text) {
                Ok(SerialClientMessage::Attach {
                    client_id,
                    port,
                    baud_rate,
                    open_if_needed,
                    pre_acquire_writer,
                    client_metadata,
                }) => {
                    attach_guard.set_target(client_id.clone(), port.clone());
                    // Open port if needed
                    if open_if_needed {
                        let open_result = await_ws_serial_open_port(
                            &port,
                            ctx.serial_manager.open_port(
                                &port,
                                baud_rate,
                                &client_id,
                                None,
                                client_metadata.clone(),
                            ),
                            WS_SERIAL_OPEN_PORT_TIMEOUT,
                        )
                        .await;
                        if let Err(message) = open_result {
                            let err_msg = SerialServerMessage::Error { message };
                            let _ = socket
                                .send(Message::Text(serialize_or_fallback(&err_msg)))
                                .await;
                            return;
                        }
                    }
                    (
                        client_id,
                        port,
                        baud_rate,
                        pre_acquire_writer,
                        client_metadata,
                    )
                }
                Ok(_) => {
                    let err_msg = SerialServerMessage::Error {
                        message: "expected attach message first".to_string(),
                    };
                    let _ = socket
                        .send(Message::Text(serialize_or_fallback(&err_msg)))
                        .await;
                    return;
                }
                Err(e) => {
                    let err_msg = SerialServerMessage::Error {
                        message: format!("invalid message: {}", e),
                    };
                    let _ = socket
                        .send(Message::Text(serialize_or_fallback(&err_msg)))
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
    let mut rx = match ctx
        .serial_manager
        .attach_reader(&port, &client_id, client_metadata.clone())
    {
        Some(rx) => rx,
        None => {
            let err_msg = SerialServerMessage::Error {
                message: format!("port {} not open", port),
            };
            let _ = socket
                .send(Message::Text(serialize_or_fallback(&err_msg)))
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
        .send(Message::Text(serialize_or_fallback(&attached)))
        .await
        .is_err()
    {
        cleanup_ws_serial_session(&ctx, &port, &client_id, writer_acquired).await;
        return;
    }
    drop(attach_guard);

    // Concurrent reader / writer / inbound split (issue #749).
    //
    // The pre-#749 implementation handled serial-RX, WebSocket-TX, and
    // WebSocket-RX in a single `tokio::select!` loop. That meant every
    // WS frame had to be flushed to the OS socket BEFORE the next
    // broadcast line could be consumed, which created head-of-line
    // blocking: while `socket.send().await` was suspended the broadcast
    // receiver couldn't drain, the broadcast channel filled to its
    // 1024-entry cap, and `RecvError::Lagged` started silently dropping
    // lines (FastLED #3219 root cause -- whole patterns vanished from
    // the wrapper's view).
    //
    // The fix splits the work three ways via `WebSocket::split()`:
    //
    //   READER (broadcast -> internal queue): pulls events from the
    //   serial broadcast channel as fast as the runtime allows. Never
    //   blocks on socket I/O. Pushes outbound messages into an
    //   unbounded mpsc queue.
    //
    //   WRITER (internal queue -> WS sink): blocks on the first
    //   `recv().await`, then non-blockingly `try_recv()`s every
    //   additional message that arrived during the previous flush.
    //   Coalesces ADJACENT `Data` messages into one Data { lines: ...,
    //   current_index } so the WS frame count stays low under bursty
    //   traffic. Non-Data messages (port events, ACKs) are emitted
    //   1:1 to preserve ordering and latency.
    //
    //   INBOUND (WS stream -> serial manager): handles client commands
    //   (Write, Detach, ClearBuffer, GetInWaiting). Routes outbound
    //   replies through the same mpsc queue so the WRITER task is the
    //   sole owner of the WS sink.
    //
    // The reader is bounded only by broadcast throughput; the writer
    // is bounded only by socket throughput. The queue absorbs the
    // mismatch, which is exactly what the device-burst case needs.
    // See FastLED/fbuild#749.

    let (out_tx, mut out_rx) = mpsc::unbounded::<SerialServerMessage>();
    let (mut ws_sink, mut ws_stream) = socket.split();
    // Inbound -> reader control channel (#756). Inbound issues Drain /
    // GetDepth requests on this; reader handles them inline alongside
    // its broadcast.recv(). Unbounded because the only producers are
    // the inbound task's ClearBuffer / GetInWaiting handlers, which
    // emit at most one message per client RPC -- bounded capacity
    // would only add deadlock corner cases for no real win.
    let (control_tx, mut control_rx) = mpsc::unbounded::<ReaderControl>();

    // READER task -- broadcast -> mpsc queue.
    let reader_handle = {
        let ctx = ctx.clone();
        let port_owned = port.clone();
        let client_id_owned = client_id.clone();
        let out_tx_reader = out_tx.clone();
        tokio::spawn(async move {
            let mut line_index: u64 = 0;
            loop {
                tokio::select! {
                    biased; // prefer broadcast events over control messages,
                            // so a burst of inbound ClearBuffer requests
                            // can't starve forwarding (control fires once
                            // per client RPC; broadcast fires per line).

                    broadcast_result = rx.recv() => match broadcast_result {
                    Ok(SerialStreamEvent::Data(line)) => {
                        ctx.touch_activity();
                        line_index += 1;
                        let mut lines: Vec<String> = Vec::with_capacity(2);
                        lines.push(line.clone());
                        if let Some(decoded) =
                            ctx.serial_manager.process_crash_line(&port_owned, &line).await
                        {
                            lines.extend(decoded);
                        }
                        let msg = SerialServerMessage::Data {
                            lines,
                            current_index: line_index,
                        };
                        if out_tx_reader.send(msg).is_err() {
                            break; // writer dropped its receiver -> session over
                        }
                    }
                    Ok(SerialStreamEvent::PortDisconnected {
                        port,
                        reason,
                        message,
                    }) => {
                        let _ = out_tx_reader.send(SerialServerMessage::PortDisconnected {
                            port,
                            reason,
                            message,
                        });
                    }
                    Ok(SerialStreamEvent::PortRenumbered {
                        port,
                        new_port,
                        reason,
                        serial,
                    }) => {
                        let _ = out_tx_reader.send(SerialServerMessage::PortRenumbered {
                            port,
                            new_port,
                            reason,
                            serial,
                        });
                    }
                    Ok(SerialStreamEvent::PortReattached {
                        port,
                        previous_port,
                    }) => {
                        let _ = out_tx_reader.send(SerialServerMessage::PortReattached {
                            port,
                            previous_port,
                        });
                    }
                    Ok(SerialStreamEvent::PortRebindFailed {
                        port,
                        new_port,
                        reason,
                        message,
                    }) => {
                        let _ = out_tx_reader.send(SerialServerMessage::PortRebindFailed {
                            port,
                            new_port,
                            reason,
                            message,
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Still a warning -- means the WRITER->socket path
                        // could not keep up with serial fan-in. With the
                        // mpsc absorber between reader and writer this
                        // should be rare; if it fires the mpsc itself is
                        // back-pressuring (unbounded; this means we ran out
                        // of memory ahead of the socket, which is a deeper
                        // problem worth surfacing).
                        tracing::warn!(
                            client_id = %client_id_owned,
                            port = %port_owned,
                            n,
                            "reader lagged at broadcast layer, skipping lines"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }, // end broadcast_result match

                    // ReaderControl branch -- inbound's ClearBuffer /
                    // GetInWaiting requests land here. Both are O(1)
                    // on the broadcast receiver, so they don't block
                    // the main forwarding loop meaningfully. The
                    // oneshot reply is best-effort: if inbound has
                    // dropped the receiver between sending the request
                    // and now (race on session teardown), the reply
                    // just goes nowhere -- inbound has already exited.
                    control_opt = control_rx.recv() => {
                        let Some(cmd) = control_opt else {
                            // All inbound senders dropped -> session
                            // teardown. Reader exits its loop too.
                            break;
                        };
                        match cmd {
                            ReaderControl::Drain { reply } => {
                                let mut drained: usize = 0;
                                while rx.try_recv().is_ok() {
                                    drained += 1;
                                }
                                let _ = reply.send(drained);
                            }
                            ReaderControl::GetDepth { reply } => {
                                let _ = reply.send(rx.len());
                            }
                        }
                    }
                } // end tokio::select!
            }
            // Dropping `out_tx_reader` here signals the writer that no more
            // data events will arrive (but inbound's clone keeps the
            // channel alive until inbound also exits).
        })
    };

    // WRITER task -- mpsc queue -> WS sink, coalescing adjacent Data.
    let writer_handle = tokio::spawn(async move {
        loop {
            // Block until at least one message is available. As soon as
            // `socket.send().await` returns from the PREVIOUS flush we
            // re-enter here and will pick up whatever the reader pushed
            // during that flush in the inner try_recv drain below.
            let Some(first) = out_rx.recv().await else {
                break; // all senders dropped
            };

            // Drain everything else that's already queued so we can pack
            // a single big WS frame on burst. `try_recv` returns
            // immediately when the queue is empty.
            let mut pending: Vec<SerialServerMessage> = Vec::with_capacity(8);
            pending.push(first);
            while let Ok(more) = out_rx.try_recv() {
                pending.push(more);
                // Soft cap so a single send doesn't grow without bound
                // when the writer is somehow far behind. Picked high
                // enough that typical bursts pack into one frame.
                if pending.len() >= 256 {
                    break;
                }
            }

            // Coalesce ADJACENT Data messages so the WS frame count is
            // O(non-data events + bursts) instead of O(lines). Non-Data
            // messages flush the current Data batch first to preserve
            // arrival order.
            let mut data_batch: Vec<String> = Vec::new();
            let mut last_index: u64 = 0;
            let mut send_failed = false;

            for msg in pending {
                match msg {
                    SerialServerMessage::Data {
                        lines,
                        current_index,
                    } => {
                        data_batch.extend(lines);
                        last_index = current_index;
                    }
                    other => {
                        if !data_batch.is_empty() {
                            let coalesced = SerialServerMessage::Data {
                                lines: std::mem::take(&mut data_batch),
                                current_index: last_index,
                            };
                            if ws_sink
                                .send(Message::Text(
                                    serde_json::to_string(&coalesced).expect(
                                        "fbuild-daemon: SerialServerMessage::Data serialization is infallible",
                                    ),
                                ))
                                .await
                                .is_err()
                            {
                                send_failed = true;
                                break;
                            }
                        }
                        if ws_sink
                            .send(Message::Text(serde_json::to_string(&other).expect(
                                "fbuild-daemon: SerialServerMessage serialization is infallible",
                            )))
                            .await
                            .is_err()
                        {
                            send_failed = true;
                            break;
                        }
                    }
                }
            }

            if !send_failed && !data_batch.is_empty() {
                let coalesced = SerialServerMessage::Data {
                    lines: data_batch,
                    current_index: last_index,
                };
                if ws_sink
                    .send(Message::Text(serde_json::to_string(&coalesced).expect(
                        "fbuild-daemon: SerialServerMessage::Data serialization is infallible",
                    )))
                    .await
                    .is_err()
                {
                    send_failed = true;
                }
            }

            if send_failed {
                break;
            }
        }
    });

    // INBOUND task -- WS stream -> serial manager + ack reply via mpsc.
    // Also owns the producer side of the #756 ReaderControl channel for
    // ClearBuffer / GetInWaiting requests.
    let inbound_handle = {
        let control_tx_inbound = control_tx;
        let ctx = ctx.clone();
        let port_owned = port.clone();
        let client_id_owned = client_id.clone();
        let out_tx_inbound = out_tx;
        tokio::spawn(async move {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<SerialClientMessage>(&text) {
                            Ok(SerialClientMessage::Write { data }) => {
                                ctx.touch_activity();
                                let decoded = match base64::engine::general_purpose::STANDARD
                                    .decode(&data)
                                {
                                    Ok(d) => d,
                                    Err(e) => {
                                        let _ = out_tx_inbound.send(SerialServerMessage::Error {
                                            message: format!("base64 decode error: {}", e),
                                        });
                                        continue;
                                    }
                                };
                                match ctx
                                    .serial_manager
                                    .write_to_port(&port_owned, &decoded, &client_id_owned)
                                    .await
                                {
                                    Ok(n) => {
                                        let _ =
                                            out_tx_inbound.send(SerialServerMessage::WriteAck {
                                                success: true,
                                                bytes_written: n,
                                                message: None,
                                            });
                                    }
                                    Err(e) => {
                                        let _ =
                                            out_tx_inbound.send(SerialServerMessage::WriteAck {
                                                success: false,
                                                bytes_written: 0,
                                                message: Some(format!("write error: {}", e)),
                                            });
                                        tracing::warn!(
                                            client_id = %client_id_owned,
                                            port = %port_owned,
                                            "write error: {}", e
                                        );
                                    }
                                }
                            }
                            Ok(SerialClientMessage::Detach) => break,
                            Ok(SerialClientMessage::ClearBuffer) => {
                                // Restored ClearBuffer semantic via the
                                // #756 ReaderControl channel: ask the
                                // reader (which owns `rx`) to drain it,
                                // await the drop-count reply, and log.
                                // Best-effort -- if reader has already
                                // exited (session teardown race), the
                                // oneshot resolves with Err and we just
                                // log debug instead of a hard error.
                                let (reply_tx, reply_rx) = oneshot::channel();
                                if control_tx_inbound
                                    .send(ReaderControl::Drain { reply: reply_tx })
                                    .is_err()
                                {
                                    tracing::debug!(
                                        client_id = %client_id_owned,
                                        port = %port_owned,
                                        "clear_buffer: reader gone, dropping request"
                                    );
                                } else {
                                    match reply_rx.await {
                                        Ok(drained) => tracing::debug!(
                                            client_id = %client_id_owned,
                                            port = %port_owned,
                                            drained,
                                            "clear_buffer drained pending lines"
                                        ),
                                        Err(_) => tracing::debug!(
                                            client_id = %client_id_owned,
                                            port = %port_owned,
                                            "clear_buffer: reader dropped reply channel"
                                        ),
                                    }
                                }
                            }
                            Ok(SerialClientMessage::GetInWaiting) => {
                                // Restored GetInWaiting semantic via the
                                // #756 ReaderControl channel: ask the
                                // reader for `rx.len()` and reply via
                                // the writer mpsc. Race-safe (reader-
                                // gone -> reply 0 honestly).
                                let (reply_tx, reply_rx) = oneshot::channel();
                                let count = if control_tx_inbound
                                    .send(ReaderControl::GetDepth { reply: reply_tx })
                                    .is_err()
                                {
                                    0
                                } else {
                                    reply_rx.await.unwrap_or(0)
                                };
                                let _ =
                                    out_tx_inbound.send(SerialServerMessage::InWaiting { count });
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!(
                                    client_id = %client_id_owned,
                                    "invalid ws message: {}", e
                                );
                            }
                        }
                    }
                    Ok(Message::Close(_)) => break,
                    Err(_) => break,
                    _ => {}
                }
            }
        })
    };

    // Wait for ANY task to exit, then abort the others. Writer dying
    // (socket error / Close) is the canonical "session over" signal.
    // Reader exits only when the broadcast channel closes (server-side
    // teardown). Inbound exits on Detach / Close / WS read error.
    tokio::select! {
        _ = writer_handle => {}
        _ = inbound_handle => {}
        _ = reader_handle => {}
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

#[cfg(test)]
#[path = "websockets_tests.rs"]
mod tests;

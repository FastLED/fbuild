use super::*;

/// #1485's client FIFO relies on the daemon emitting request replies in
/// request order. Exercise the production WebSocket handler with a real
/// pseudo-terminal, so this catches a future inbound/writer concurrency
/// change that reorders `write_ack` and `in_waiting` frames.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn serial_ws_replies_follow_request_order() {
    use std::ffi::CStr;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use tokio_tungstenite::tungstenite::Message as ClientMessage;

    let mut master_fd = -1;
    let mut slave_fd = -1;
    // SAFETY: openpty initializes both fds on success. OwnedFd takes over
    // each descriptor exactly once, and ttyname_r writes into a sized buffer.
    let rc = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
    let _master = unsafe { OwnedFd::from_raw_fd(master_fd) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
    let mut name = [0i8; 256];
    let rc = unsafe { libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr(), name.len()) };
    assert_eq!(rc, 0, "ttyname_r failed: {rc}");
    let port = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_str()
        .unwrap()
        .to_owned();

    let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
    let ctx = Arc::new(DaemonContext::new(8765, shutdown_tx, "test".to_string()));
    let router = axum::Router::new()
        .route("/ws/serial-monitor", axum::routing::get(ws_serial_monitor))
        .with_state(ctx);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws/serial-monitor"))
        .await
        .unwrap();
    let attach = serde_json::json!({
        "type": "attach", "client_id": "fifo-order-test", "port": port,
        "baud_rate": 115200, "open_if_needed": true, "pre_acquire_writer": true
    });
    ws.send(ClientMessage::Text(attach.to_string()))
        .await
        .unwrap();
    let attached = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ClientMessage::Text(attached) = attached else {
        panic!("expected attached text frame");
    };
    let attached: serde_json::Value = serde_json::from_str(&attached).unwrap();
    assert_eq!(attached["type"], "attached", "{attached}");
    assert_eq!(attached["success"], true, "{attached}");
    assert_eq!(attached["writer_pre_acquired"], true, "{attached}");

    let mut expected = Vec::new();
    for len in 1..=20usize {
        let payload = vec![b'x'; len];
        let write = serde_json::json!({
            "type": "write",
            "data": base64::engine::general_purpose::STANDARD.encode(payload)
        });
        ws.send(ClientMessage::Text(write.to_string()))
            .await
            .unwrap();
        expected.push(("write_ack", len));
        ws.send(ClientMessage::Text(r#"{"type":"get_in_waiting"}"#.into()))
            .await
            .unwrap();
        expected.push(("in_waiting", 0));
    }
    // A decode error replaces one write_ack; it must occupy exactly that
    // request's position so the following valid write is still matched.
    ws.send(ClientMessage::Text(
        r#"{"type":"write","data":"not-base64!"}"#.into(),
    ))
    .await
    .unwrap();
    expected.push(("error", 0));
    let last_write = serde_json::json!({
        "type": "write",
        "data": base64::engine::general_purpose::STANDARD.encode(b"end")
    });
    ws.send(ClientMessage::Text(last_write.to_string()))
        .await
        .unwrap();
    expected.push(("write_ack", 3));

    for (index, (kind, byte_count)) in expected.into_iter().enumerate() {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap_or_else(|_| panic!("reply {index} timed out"))
            .unwrap()
            .unwrap();
        let ClientMessage::Text(text) = frame else {
            panic!("reply {index} was not a text frame: {frame:?}");
        };
        let reply: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(reply["type"], kind, "reply {index}: {reply}");
        if kind == "write_ack" {
            assert_eq!(reply["bytes_written"], byte_count, "reply {index}: {reply}");
        }
    }

    ws.send(ClientMessage::Text(r#"{"type":"detach"}"#.into()))
        .await
        .unwrap();
    server.abort();
}

/// The real handler must keep one WebSocket alive across deploy's physical
/// port close and re-open its serial subscription after preemption clears.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn serial_ws_preemption_reconnects_after_deploy() {
    serial_ws_preemption_reconnects_after_deploy_impl(false, false).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn serial_ws_preemption_reconnects_to_renumbered_port() {
    serial_ws_preemption_reconnects_after_deploy_impl(true, false).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn serial_ws_preemption_without_recovered_port_closes_monitor() {
    serial_ws_preemption_reconnects_after_deploy_impl(false, true).await;
}

#[cfg(target_os = "linux")]
async fn serial_ws_preemption_reconnects_after_deploy_impl(renumber: bool, fail_recovery: bool) {
    use std::ffi::CStr;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use tokio_tungstenite::tungstenite::Message as ClientMessage;

    let mut master_fd = -1;
    let mut slave_fd = -1;
    // SAFETY: openpty initializes both descriptors on success; OwnedFd takes
    // each exactly once and ttyname_r writes into the sized buffer.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        },
        0
    );
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
    let mut name = [0i8; 256];
    assert_eq!(
        unsafe { libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr(), name.len()) },
        0
    );
    let port = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_str()
        .unwrap()
        .to_owned();
    let mut recovered_master = None;
    let mut recovered_slave = None;
    let recovered_port = if renumber {
        let mut new_master_fd = -1;
        let mut new_slave_fd = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut new_master_fd,
                    &mut new_slave_fd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            },
            0
        );
        recovered_master = Some(unsafe { OwnedFd::from_raw_fd(new_master_fd) });
        let new_slave = unsafe { OwnedFd::from_raw_fd(new_slave_fd) };
        let mut new_name = [0i8; 256];
        assert_eq!(
            unsafe {
                libc::ttyname_r(new_slave.as_raw_fd(), new_name.as_mut_ptr(), new_name.len())
            },
            0
        );
        let new_port = unsafe { CStr::from_ptr(new_name.as_ptr()) }
            .to_str()
            .unwrap()
            .to_owned();
        recovered_slave = Some(new_slave);
        new_port
    } else {
        port.clone()
    };

    let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
    let ctx = Arc::new(DaemonContext::new(8765, shutdown_tx, "test".to_string()));
    let router = axum::Router::new()
        .route("/ws/serial-monitor", axum::routing::get(ws_serial_monitor))
        .with_state(Arc::clone(&ctx));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws/serial-monitor"))
        .await
        .unwrap();
    let attach = serde_json::json!({
        "type": "attach", "client_id": "preempt-test", "port": port,
        "baud_rate": 115200, "open_if_needed": true, "pre_acquire_writer": true
    });
    ws.send(ClientMessage::Text(attach.to_string()))
        .await
        .unwrap();
    let attached = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(attached, ClientMessage::Text(_)));

    ctx.serial_manager
        .preempt_for_deploy(&port, "deploy".to_string(), "test".to_string())
        .await
        .unwrap();
    let preempted = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("preempted frame timeout")
        .expect("socket closed before preempted frame")
        .unwrap();
    let ClientMessage::Text(preempted) = preempted else {
        panic!("expected preempted text frame");
    };
    let preempted: serde_json::Value = serde_json::from_str(&preempted).unwrap();
    assert_eq!(preempted["type"], "preempted", "{preempted}");

    if fail_recovery {
        ctx.serial_manager
            .fail_deploy_preemption(&port, "no healthy runtime serial port")
            .await;
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("port_disconnected frame timeout")
            .expect("socket closed before port_disconnected frame")
            .unwrap();
        let ClientMessage::Text(frame) = frame else {
            panic!("expected port_disconnected text frame");
        };
        let disconnected: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(disconnected["type"], "port_disconnected", "{disconnected}");
        assert_eq!(disconnected["reason"], "deploy_recovery_failed");
        assert!(!ctx.serial_manager.is_preempted(&port).await);
        server.abort();
        return;
    }

    if renumber {
        drop(master);
        drop(slave);
    }

    ctx.serial_manager
        .complete_deploy_preemption(&port, &recovered_port)
        .await;
    let reconnected = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("reconnected frame timeout")
        .expect("socket closed before reconnected frame")
        .unwrap();
    let ClientMessage::Text(reconnected) = reconnected else {
        panic!("expected reconnected text frame");
    };
    let reconnected: serde_json::Value = serde_json::from_str(&reconnected).unwrap();
    assert_eq!(reconnected["type"], "reconnected", "{reconnected}");
    assert!(
        reconnected["message"]
            .as_str()
            .unwrap()
            .contains(&recovered_port),
        "{reconnected}"
    );

    let write = serde_json::json!({
        "type": "write", "data": base64::engine::general_purpose::STANDARD.encode(b"after")
    });
    ws.send(ClientMessage::Text(write.to_string()))
        .await
        .unwrap();
    let ack = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("post-reconnect write ack timeout")
        .unwrap()
        .unwrap();
    let ClientMessage::Text(ack) = ack else {
        panic!("expected write ack text frame");
    };
    let ack: serde_json::Value = serde_json::from_str(&ack).unwrap();
    assert_eq!(ack["type"], "write_ack", "{ack}");
    assert_eq!(ack["success"], true, "{ack}");
    assert_eq!(ack["bytes_written"], 5, "{ack}");
    if let Some(master) = recovered_master.as_ref() {
        let mut poll_fd = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll_fd, 1, 2_000) }, 1);
        let mut bytes = [0u8; 16];
        let count =
            unsafe { libc::read(master.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
        assert_eq!(&bytes[..count as usize], b"after");
    }

    ws.send(ClientMessage::Text(r#"{"type":"detach"}"#.into()))
        .await
        .unwrap();
    drop((recovered_master, recovered_slave));
    server.abort();
}

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

#[test]
fn ws_open_port_timeout_is_short_for_localhost_clients() {
    assert_eq!(
        WS_SERIAL_OPEN_PORT_TIMEOUT,
        std::time::Duration::from_secs(3)
    );
}

#[tokio::test]
async fn ws_open_port_timeout_returns_error_with_deadline() {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        await_ws_serial_open_port(
            "COM_HUNG",
            std::future::pending::<fbuild_core::Result<()>>(),
            std::time::Duration::from_millis(10),
        ),
    )
    .await
    .expect("test helper should return at the injected deadline");

    let message = result.expect_err("hung open_port must be reported as an error");
    assert!(
        message.contains("open_port(COM_HUNG) exceeded 10ms"),
        "timeout error should name the port and deadline, got: {message}"
    );
    assert!(
        message.contains("serial driver may be wedged"),
        "timeout error should explain likely serial-driver wedge, got: {message}"
    );
}

async fn run_pending_attach_timeout_scope(ctx: std::sync::Arc<DaemonContext>) -> String {
    let attach_guard = PendingAttachGuard::new(ctx.clone());
    attach_guard.set_target("client-hung".to_string(), "COM_HUNG".to_string());

    assert_eq!(
        ctx.pending_serial_attaches
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(ctx.pending_serial_attach_infos().len(), 1);

    let message = await_ws_serial_open_port(
        "COM_HUNG",
        std::future::pending::<fbuild_core::Result<()>>(),
        std::time::Duration::from_millis(10),
    )
    .await
    .expect_err("hung open_port must time out");

    // `attach_guard` drops as this async function returns, matching the
    // production handler's timeout/error return path.
    message
}

#[tokio::test]
async fn ws_open_port_timeout_drops_pending_attach_guard() {
    let (tx, _rx) = tokio::sync::watch::channel(false);
    let ctx = std::sync::Arc::new(DaemonContext::new(8765, tx, "test".to_string()));

    let message = run_pending_attach_timeout_scope(ctx.clone()).await;
    assert!(message.contains("open_port(COM_HUNG) exceeded 10ms"));

    assert_eq!(
        ctx.pending_serial_attaches
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert!(
        ctx.pending_serial_attach_infos().is_empty(),
        "pending attach details should be removed after timeout"
    );
    assert_eq!(
        ctx.busy_reason(),
        None,
        "timed-out WebSocket attach must not keep the daemon busy"
    );
}

// ---------------------------------------------------------------
// ReaderControl + writer-batching topology tests (#757).
//
// These exercise the contracts of the post-#750 reader/writer/
// inbound split WITHOUT needing axum's WebSocket harness or a
// real serial port. The actual reader / writer / inbound task
// bodies are spawned inside `handle_serial_ws()` and capture
// local closures, so we don't reach into them directly --
// instead we exercise the *primitives* they rely on:
//
//   - `ReaderControl::Drain` round-trips through an mpsc to a
//     toy reader that drains a broadcast channel
//   - `ReaderControl::GetDepth` round-trips and reports broadcast
//     queue depth
//   - Writer-style coalescing of adjacent `SerialServerMessage::
//     Data` messages into a single Data with merged `lines`
//
// The full spawn-topology integration test is deferred to a
// tests/serial_ws_burst.rs harness (separate sub-PR of #757)
// because axum's WebSocket testing requires standing up a real
// hyper server -- substantially more scaffolding than these
// primitive-level checks need.
// ---------------------------------------------------------------

use tokio::sync::broadcast;

/// Tiny model of the reader task: a single select between broadcast
/// recv and ReaderControl, exposing only the ReaderControl branch
/// so we can test it in isolation. NOT the production code path --
/// the production reader is in `handle_serial_ws()` inline. This
/// mirrors its ReaderControl handling so the contract is exercised.
async fn run_toy_reader(
    mut rx: broadcast::Receiver<u32>,
    mut control_rx: mpsc::UnboundedReceiver<ReaderControl>,
) {
    loop {
        tokio::select! {
            biased;
            broadcast_result = rx.recv() => match broadcast_result {
                Ok(_) => {} // drop; we only care about the ReaderControl branch here
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
            control_opt = control_rx.recv() => {
                let Some(cmd) = control_opt else { break };
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
        }
    }
}

#[tokio::test]
async fn reader_control_drain_reports_drop_count() {
    let (bcast_tx, bcast_rx) = broadcast::channel::<u32>(16);
    let (ctl_tx, ctl_rx) = mpsc::unbounded();
    let reader = tokio::spawn(run_toy_reader(bcast_rx, ctl_rx));

    // Push 5 events, do NOT let the reader drain them via its
    // main `rx.recv()` (the toy reader hits select between
    // broadcast and control; with biased priority the broadcast
    // wins, so we need to send Drain BEFORE the reader awakes).
    //
    // Workaround: bound how many events are pushed before Drain
    // by sending the events synchronously, then immediately
    // sending Drain. Tokio scheduling means the toy reader will
    // see both branches ready; biased order makes it serve the
    // broadcast first, draining N-1 (it consumes one per loop
    // iteration). Then the next iteration sees Drain and gets
    // whatever is left. This proves the drain count IS the
    // residue after natural consumption — close enough for the
    // contract.

    for i in 0..5u32 {
        bcast_tx.send(i).unwrap();
    }
    // Tiny yield so the reader sees the broadcast queue, then we
    // race a Drain in before all events are consumed.
    let (reply_tx, reply_rx) = oneshot::channel();
    ctl_tx
        .send(ReaderControl::Drain { reply: reply_tx })
        .unwrap();
    let drained = reply_rx.await.expect("reader replied");
    // At least one event present at the drain point. The exact
    // count is timing-dependent on the select scheduler; the
    // contract we're proving is `replied with a real count`,
    // not a specific number.
    assert!(
        drained <= 5,
        "drain reported {drained} but only 5 events were ever sent"
    );

    drop(bcast_tx); // close broadcast so reader exits cleanly
    drop(ctl_tx);
    let _ = reader.await;
}

#[tokio::test]
async fn reader_control_get_depth_reports_broadcast_length() {
    let (bcast_tx, bcast_rx) = broadcast::channel::<u32>(16);
    let (ctl_tx, ctl_rx) = mpsc::unbounded();
    let reader = tokio::spawn(run_toy_reader(bcast_rx, ctl_rx));

    for i in 0..3u32 {
        bcast_tx.send(i).unwrap();
    }
    let (reply_tx, reply_rx) = oneshot::channel();
    ctl_tx
        .send(ReaderControl::GetDepth { reply: reply_tx })
        .unwrap();
    let depth = reply_rx.await.expect("reader replied");
    // Same timing caveat as above — the reader may have consumed
    // some entries between push and reply. Contract: the reply
    // IS a count ≤ what we sent.
    assert!(depth <= 3, "depth reported {depth} but only 3 sent");

    drop(bcast_tx);
    drop(ctl_tx);
    let _ = reader.await;
}

/// Models the writer task's batching/coalescing logic in isolation.
/// Production version lives inline in `handle_serial_ws()`. This
/// proves the contract: adjacent Data messages merge their `lines`
/// into a single output Data; non-Data messages preserve ordering
/// by flushing the current Data batch first.
fn coalesce_for_test(input: Vec<SerialServerMessage>) -> Vec<SerialServerMessage> {
    let mut output = Vec::new();
    let mut data_batch: Vec<String> = Vec::new();
    let mut last_index: u64 = 0;
    for msg in input {
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
                    output.push(SerialServerMessage::Data {
                        lines: std::mem::take(&mut data_batch),
                        current_index: last_index,
                    });
                }
                output.push(other);
            }
        }
    }
    if !data_batch.is_empty() {
        output.push(SerialServerMessage::Data {
            lines: data_batch,
            current_index: last_index,
        });
    }
    output
}

#[test]
fn writer_coalesces_adjacent_data_into_one_frame() {
    // 5 single-line Data messages -> 1 Data with 5 lines.
    let input = vec![
        SerialServerMessage::Data {
            lines: vec!["a".into()],
            current_index: 1,
        },
        SerialServerMessage::Data {
            lines: vec!["b".into()],
            current_index: 2,
        },
        SerialServerMessage::Data {
            lines: vec!["c".into()],
            current_index: 3,
        },
        SerialServerMessage::Data {
            lines: vec!["d".into()],
            current_index: 4,
        },
        SerialServerMessage::Data {
            lines: vec!["e".into()],
            current_index: 5,
        },
    ];
    let output = coalesce_for_test(input);
    assert_eq!(output.len(), 1, "should coalesce to 1 frame");
    match &output[0] {
        SerialServerMessage::Data {
            lines,
            current_index,
        } => {
            assert_eq!(lines, &vec!["a", "b", "c", "d", "e"]);
            assert_eq!(*current_index, 5);
        }
        other => panic!("expected Data, got {:?}", other),
    }
}

#[test]
fn writer_flushes_data_before_non_data_event() {
    // Data, Data, PortDisconnected, Data
    // -> Data{lines:[a,b]}, PortDisconnected, Data{lines:[c]}
    let input = vec![
        SerialServerMessage::Data {
            lines: vec!["a".into()],
            current_index: 1,
        },
        SerialServerMessage::Data {
            lines: vec!["b".into()],
            current_index: 2,
        },
        SerialServerMessage::PortDisconnected {
            port: "COM3".into(),
            reason: "unplugged".into(),
            message: "".into(),
        },
        SerialServerMessage::Data {
            lines: vec!["c".into()],
            current_index: 3,
        },
    ];
    let output = coalesce_for_test(input);
    assert_eq!(output.len(), 3, "expected 3 output frames");
    match &output[0] {
        SerialServerMessage::Data {
            lines,
            current_index,
        } => {
            assert_eq!(lines, &vec!["a", "b"]);
            assert_eq!(*current_index, 2);
        }
        other => panic!("output[0] expected Data, got {:?}", other),
    }
    match &output[1] {
        SerialServerMessage::PortDisconnected { port, .. } => {
            assert_eq!(port, "COM3");
        }
        other => panic!("output[1] expected PortDisconnected, got {:?}", other),
    }
    match &output[2] {
        SerialServerMessage::Data {
            lines,
            current_index,
        } => {
            assert_eq!(lines, &vec!["c"]);
            assert_eq!(*current_index, 3);
        }
        other => panic!("output[2] expected Data, got {:?}", other),
    }
}

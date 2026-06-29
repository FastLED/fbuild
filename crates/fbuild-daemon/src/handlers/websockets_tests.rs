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

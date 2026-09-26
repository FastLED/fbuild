use super::*;
use std::sync::atomic::AtomicU64;
use tokio::net::TcpListener;

/// Small deterministic PRNG shared by the fake daemon and AT-19 so a
/// failed soak can replay its exact delays and operation mix.
struct Xorshift64(u64);

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
}

/// Fault hooks the fake daemon can be told to trigger.
#[derive(Default, Clone)]
struct DaemonKnobs {
    echo_delay: Duration,
    ack_delay: Duration,
    chaos_seed: Option<u64>,
    preempt_on_nth_write: Option<usize>,
    disconnect_after: Option<Duration>,
    fail_ack_on_nth_write: Option<usize>,
    inject_error_on_nth_write: Option<usize>,
    stop_reading_after_writes: Option<usize>,
    bulk_lines_on_attach: usize,
    suppress_write_echo: bool,
}

async fn fake_daemon(listener: TcpListener, knobs: DaemonKnobs) {
    let (stream, _) = listener.accept().await.unwrap();
    stream.set_nodelay(true).unwrap();
    let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
    let (sink, mut source) = ws.split();
    let sink = Arc::new(tokio::sync::Mutex::new(sink));
    let attach_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let write_count = Arc::new(AtomicU64::new(0));

    while let Some(Ok(msg)) = source.next().await {
        let tungstenite::Message::Text(text) = msg else {
            continue;
        };
        let request: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !attach_seen.load(Ordering::Relaxed) {
            attach_seen.store(true, Ordering::Relaxed);
            let ack = serde_json::json!({
                "type": "attached", "success": true, "message": "ok", "writer_pre_acquired": true
            });
            sink.lock()
                .await
                .send(tungstenite::Message::Text(ack.to_string()))
                .await
                .unwrap();
            if let Some(delay) = knobs.disconnect_after {
                let sink = Arc::clone(&sink);
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    let disconnected = serde_json::json!({
                        "type": "port_disconnected", "port": "COM_TEST",
                        "reason": "test_disconnect", "message": "chaos soak"
                    });
                    let mut sink = sink.lock().await;
                    let _ = sink
                        .send(tungstenite::Message::Text(disconnected.to_string()))
                        .await;
                    let _ = sink.send(tungstenite::Message::Close(None)).await;
                });
            }
            if knobs.stop_reading_after_writes == Some(0) {
                std::future::pending::<()>().await;
            }
            if knobs.bulk_lines_on_attach > 0 {
                let bulk_sink = Arc::clone(&sink);
                let total = knobs.bulk_lines_on_attach;
                tokio::spawn(async move {
                    for start in (0..total).step_by(1_000) {
                        let end = (start + 1_000).min(total);
                        let lines: Vec<String> =
                            (start..end).map(|i| format!("line-{i}")).collect();
                        let frame = serde_json::json!({
                            "type": "data", "lines": lines, "current_index": end
                        });
                        if bulk_sink
                            .lock()
                            .await
                            .send(tungstenite::Message::Text(frame.to_string()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                });
            }
            continue;
        }
        match request["type"].as_str() {
            Some("write") => {
                let n = write_count.fetch_add(1, Ordering::SeqCst) + 1;
                if let Some(stop_after) = knobs.stop_reading_after_writes {
                    if n as usize > stop_after {
                        // Simulate a peer that stops reading: never ack,
                        // never disconnect.
                        std::future::pending::<()>().await;
                    }
                }
                let data = request["data"].as_str().unwrap().to_string();
                let delay = knobs.chaos_seed.map_or(knobs.ack_delay, |seed| {
                    let mut rng = Xorshift64::new(seed ^ n);
                    Duration::from_millis(rng.below(51))
                });
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                if knobs.preempt_on_nth_write == Some(n as usize) {
                    let preempted = serde_json::json!({
                        "type": "preempted", "reason": "deploy", "preempted_by": "test"
                    });
                    let reconnected = serde_json::json!({
                        "type": "reconnected", "message": "ready"
                    });
                    let mut sink = sink.lock().await;
                    if sink
                        .send(tungstenite::Message::Text(preempted.to_string()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if sink
                        .send(tungstenite::Message::Text(reconnected.to_string()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                if knobs.inject_error_on_nth_write == Some(n as usize)
                    || (knobs.chaos_seed.is_some() && n.is_multiple_of(23))
                {
                    let err = serde_json::json!({"type": "error", "message": "bad base64"});
                    if sink
                        .lock()
                        .await
                        .send(tungstenite::Message::Text(err.to_string()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                if knobs.fail_ack_on_nth_write == Some(n as usize) {
                    let ack = serde_json::json!({
                        "type": "write_ack", "success": false,
                        "bytes_written": 0, "message": "port write failed"
                    });
                    if sink
                        .lock()
                        .await
                        .send(tungstenite::Message::Text(ack.to_string()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                let decoded = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    data.as_bytes(),
                )
                .unwrap_or_default();
                let ack = serde_json::json!({
                    "type": "write_ack", "success": true, "bytes_written": decoded.len(), "message": null
                });
                if sink
                    .lock()
                    .await
                    .send(tungstenite::Message::Text(ack.to_string()))
                    .await
                    .is_err()
                {
                    return;
                }
                if knobs.suppress_write_echo {
                    continue;
                }
                let text = String::from_utf8_lossy(&decoded).trim().to_string();
                let echoed = if text.starts_with(REMOTE_PREFIX) {
                    text
                } else {
                    format!("echo:{text}")
                };
                let line =
                    serde_json::json!({ "type": "data", "lines": [echoed], "current_index": 0 })
                        .to_string();
                if knobs.echo_delay.is_zero() {
                    if sink
                        .lock()
                        .await
                        .send(tungstenite::Message::Text(line))
                        .await
                        .is_err()
                    {
                        return;
                    }
                } else {
                    let sink = Arc::clone(&sink);
                    let delay = knobs.echo_delay;
                    tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        let _ = sink
                            .lock()
                            .await
                            .send(tungstenite::Message::Text(line))
                            .await;
                    });
                }
            }
            Some("get_in_waiting") => {
                let reply = serde_json::json!({ "type": "in_waiting", "count": 0 });
                if sink
                    .lock()
                    .await
                    .send(tungstenite::Message::Text(reply.to_string()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Some("clear_buffer") | Some("detach") => {}
            _ => {}
        }
    }
}

async fn connect_to(knobs: DaemonKnobs) -> (SerialSession, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(fake_daemon(listener, knobs));
    let cfg = SessionConfig {
        ws_url: format!("ws://127.0.0.1:{port}"),
        port: "COM_TEST".into(),
        baud_rate: 115200,
        auto_reconnect: true,
        verbose: false,
        client_id: "test".into(),
        max_buffered_lines: DEFAULT_MAX_BUFFERED_LINES,
        handshake_timeout: Duration::from_secs(5),
    };
    let session = SerialSession::connect(cfg).await.expect("connect");
    (session, port)
}

fn assert_line_accounting(session: &SerialSession, seed: u64, generation: &str) {
    let inner = &session.inner;
    let received = inner.lines_received_for_test.load(Ordering::Relaxed);
    let delivered = inner.lines_delivered_for_test.load(Ordering::Relaxed);
    let discarded = inner.lines_discarded_for_test.load(Ordering::Relaxed);
    let overflowed = session.lines_dropped();
    let queued = inner
        .line_queue
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .len();
    assert_eq!(
        received,
        delivered + discarded + overflowed + queued,
        "line accounting mismatch in {generation} session (seed={seed}): received={received}, delivered={delivered}, discarded={discarded}, overflowed={overflowed}, queued={queued}"
    );
}

/// §5.3 watchdog: wraps a test body in a budget; on expiry, panics with
/// core-state evidence instead of letting CI's job timeout eat it.
async fn with_watchdog<F: std::future::Future<Output = ()>>(budget: Duration, fut: F) {
    match tokio::time::timeout(budget, fut).await {
        Ok(()) => {}
        Err(_) => panic!("test exceeded its {budget:?} watchdog budget"),
    }
}

#[tokio::test]
async fn at7_failed_write_ack_is_an_error_not_a_zero_byte_success() {
    with_watchdog(Duration::from_secs(10), async {
        let (session, _) = connect_to(DaemonKnobs {
            fail_ack_on_nth_write: Some(1),
            ..Default::default()
        })
        .await;
        let result = session.write(b"hello", Duration::from_secs(2)).await;
        assert!(
            result.is_err(),
            "daemon write_ack(success=false) must be a typed error, got {result:?}"
        );
        assert_eq!(session.write(b"next", Duration::from_secs(2)).await, Ok(4));
        session.close().await;
    })
    .await;
}

#[tokio::test]
async fn closed_session_cannot_be_revived_by_late_reconnect() {
    with_watchdog(Duration::from_secs(10), async {
        let (session, _) = connect_to(DaemonKnobs::default()).await;
        session.inner.set_status(SessionStatus::Preempted);
        session.close().await;
        session.inner.reconnect_if_preempted();
        assert_eq!(session.inner.status(), SessionStatus::Closed);
    })
    .await;
}

#[tokio::test]
async fn at1_write_during_long_read_does_not_stall() {
    with_watchdog(Duration::from_secs(20), async {
        let (session, _) = connect_to(DaemonKnobs {
            echo_delay: Duration::from_millis(20),
            ..Default::default()
        })
        .await;
        let session = Arc::new(session);
        let reader = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.read_lines(Duration::from_secs(10)).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = tokio::time::Instant::now();
        let n = session
            .write(b"ping", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(n, 4);
        assert!(started.elapsed() < Duration::from_secs(1));
        let lines = tokio::time::timeout(Duration::from_secs(5), reader)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lines, ["echo:ping"]);
    })
    .await;
}

#[tokio::test]
async fn at2_json_rpc_reply_not_stolen_by_concurrent_reader() {
    with_watchdog(Duration::from_secs(20), async {
        let (session, _) = connect_to(DaemonKnobs {
            echo_delay: Duration::from_millis(150),
            ..Default::default()
        })
        .await;
        let session = Arc::new(session);
        let reader = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.read_lines(Duration::from_secs(10)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let reply = session
            .json_rpc("REMOTE:{\"id\":1}", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(reply.trim(), "{\"id\":1}");
        let lines = reader.await.unwrap();
        assert!(
            lines.iter().all(|l| !l.starts_with(REMOTE_PREFIX)),
            "reader must never see a REMOTE: line: {lines:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn at3_concurrent_writers_each_get_their_own_ack() {
    with_watchdog(Duration::from_secs(30), async {
        let (session, _) = connect_to(DaemonKnobs::default()).await;
        let session = Arc::new(session);
        for _round in 0..5 {
            let mut handles = Vec::new();
            for len in 1..=32usize {
                let session = Arc::clone(&session);
                handles.push(tokio::spawn(async move {
                    let payload = "x".repeat(len);
                    let n = session
                        .write(payload.as_bytes(), Duration::from_secs(5))
                        .await
                        .unwrap();
                    (len, n)
                }));
            }
            for h in handles {
                let (len, n) = h.await.unwrap();
                assert_eq!(n, len, "a write got another write's ack");
            }
        }
    })
    .await;
}

#[tokio::test]
async fn at4_write_and_in_waiting_interleaved() {
    with_watchdog(Duration::from_secs(20), async {
        let (session, _) = connect_to(DaemonKnobs::default()).await;
        let session = Arc::new(session);
        let mut handles = Vec::new();
        for i in 0..16 {
            let session = Arc::clone(&session);
            if i % 2 == 0 {
                handles.push(tokio::spawn(async move {
                    session
                        .write(b"ab", Duration::from_secs(5))
                        .await
                        .map(|_| ())
                }));
            } else {
                handles.push(tokio::spawn(async move {
                    session.in_waiting(Duration::from_secs(5)).await.map(|_| ())
                }));
            }
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
    })
    .await;
}

#[tokio::test]
async fn at5_error_frame_does_not_shift_the_fifo() {
    with_watchdog(Duration::from_secs(10), async {
        let (session, _) = connect_to(DaemonKnobs {
            inject_error_on_nth_write: Some(1),
            ..Default::default()
        })
        .await;
        let first = session.write(b"a", Duration::from_secs(2)).await;
        assert_eq!(first, Err(SessionError::ProtocolDesync));
        let second = session.write(b"bb", Duration::from_secs(2)).await.unwrap();
        assert_eq!(second, 2, "the next write must get its own ack");
    })
    .await;
}

#[tokio::test]
async fn at6_cancelling_read_lines_loses_nothing() {
    with_watchdog(Duration::from_secs(30), async {
        let (session, _) = connect_to(DaemonKnobs::default()).await;
        let session = Arc::new(session);
        let total_sent = 1_000usize;
        let sender = {
            let session = Arc::clone(&session);
            tokio::spawn(async move {
                for i in 0..total_sent {
                    session
                        .write(format!("m{i}").as_bytes(), Duration::from_secs(2))
                        .await
                        .unwrap();
                }
            })
        };
        let mut delivered = Vec::new();
        for iteration in 0..1_000 {
            let fut = session.read_lines(Duration::from_secs(2));
            if iteration % 2 == 0 {
                if let Ok(lines) = tokio::time::timeout(Duration::from_millis(1), fut).await {
                    delivered.extend(lines);
                }
            } else {
                tokio::pin!(fut);
                tokio::select! {
                    lines = &mut fut => delivered.extend(lines),
                    () = tokio::time::sleep(Duration::from_millis(1)) => {}
                }
            }
        }
        sender.await.unwrap();
        // Drain anything left over after the cancel storm.
        loop {
            let batch = session.read_lines(Duration::from_millis(200)).await;
            if batch.is_empty() {
                break;
            }
            delivered.extend(batch);
        }
        let expected: Vec<String> = (0..total_sent).map(|i| format!("echo:m{i}")).collect();
        assert_eq!(delivered, expected, "lines lost, reordered or duplicated");
    })
    .await;
}

#[tokio::test]
async fn at7_late_ack_after_timeout_is_discarded() {
    with_watchdog(Duration::from_secs(10), async {
        let (session, _) = connect_to(DaemonKnobs {
            ack_delay: Duration::from_millis(300),
            ..Default::default()
        })
        .await;
        let timed_out = session.write(b"a", Duration::from_millis(50)).await;
        assert_eq!(timed_out, Err(SessionError::Timeout));
        // Let the late ack land, then issue a second write.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let second = session.write(b"bb", Duration::from_secs(2)).await.unwrap();
        assert_eq!(second, 2);
    })
    .await;
}

#[tokio::test]
async fn at8_late_rpc_reply_is_not_given_to_next_request() {
    with_watchdog(Duration::from_secs(10), async {
        let (session, _) = connect_to(DaemonKnobs {
            echo_delay: Duration::from_millis(200),
            ..Default::default()
        })
        .await;
        let first = session
            .json_rpc("REMOTE:{\"id\":1}", Duration::from_millis(50))
            .await;
        assert_eq!(first, Err(SessionError::Timeout));
        let second = session
            .json_rpc("REMOTE:{\"id\":2}", Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(second, "{\"id\":2}");
    })
    .await;
}

#[tokio::test]
async fn error_ack_removes_only_its_rpc_waiter() {
    with_watchdog(Duration::from_secs(10), async {
        let (session, _) = connect_to(DaemonKnobs {
            inject_error_on_nth_write: Some(1),
            ..Default::default()
        })
        .await;
        let first = session
            .json_rpc("REMOTE:{\"id\":1}", Duration::from_secs(1))
            .await;
        assert_eq!(first, Err(SessionError::ProtocolDesync));
        let second = session
            .json_rpc("REMOTE:{\"id\":2}", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(second, "{\"id\":2}");
    })
    .await;
}

#[tokio::test]
async fn at9_preempted_write_fails_fast_without_auto_reconnect() {
    with_watchdog(Duration::from_secs(10), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut sink, _source) = ws.split();
                let ack = serde_json::json!({"type":"attached","success":true,"message":"ok","writer_pre_acquired":true});
                sink.send(tungstenite::Message::Text(ack.to_string()))
                    .await
                    .unwrap();
                let preempted =
                    serde_json::json!({"type":"preempted","reason":"deploy","preempted_by":"x"});
                sink.send(tungstenite::Message::Text(preempted.to_string()))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(150)).await;
                let reconnected = serde_json::json!({"type":"reconnected","message":"ready"});
                sink.send(tungstenite::Message::Text(reconnected.to_string()))
                    .await
                    .unwrap();
                std::future::pending::<()>().await;
            });
            let cfg = SessionConfig {
                ws_url: format!("ws://127.0.0.1:{port}"),
                port: "COM_TEST".into(),
                baud_rate: 115200,
                auto_reconnect: false,
                verbose: false,
                client_id: "test".into(),
                max_buffered_lines: DEFAULT_MAX_BUFFERED_LINES,
                handshake_timeout: Duration::from_secs(5),
            };
            let session = SerialSession::connect(cfg).await.unwrap();
            // Give the preempted frame time to be processed.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let started = tokio::time::Instant::now();
            let result = session.write(b"x", Duration::from_secs(10)).await;
            assert_eq!(result, Err(SessionError::Preempted));
            assert!(started.elapsed() < Duration::from_secs(1));
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(session.inner.status(), SessionStatus::Preempted);
            assert_eq!(session.write(b"again", Duration::from_secs(1)).await, Err(SessionError::Preempted));
        })
        .await;
}

#[tokio::test]
async fn at9_auto_reconnect_keeps_reader_waiting_but_rejects_write() {
    with_watchdog(Duration::from_secs(10), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut sink, _source) = ws.split();
                for frame in [
                    serde_json::json!({"type":"attached","success":true,"message":"ok","writer_pre_acquired":true}),
                    serde_json::json!({"type":"preempted","reason":"deploy","preempted_by":"x"}),
                ] {
                    sink.send(tungstenite::Message::Text(frame.to_string()))
                        .await
                        .unwrap();
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
                for frame in [
                    serde_json::json!({"type":"reconnected","message":"ok"}),
                    serde_json::json!({"type":"data","lines":["after-reconnect"],"current_index":0}),
                ] {
                    sink.send(tungstenite::Message::Text(frame.to_string()))
                        .await
                        .unwrap();
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            });
            let session = Arc::new(
                SerialSession::connect(SessionConfig {
                    ws_url: format!("ws://127.0.0.1:{port}"),
                    port: "COM_TEST".into(),
                    baud_rate: 115200,
                    auto_reconnect: true,
                    verbose: false,
                    client_id: "test".into(),
                    max_buffered_lines: DEFAULT_MAX_BUFFERED_LINES,
                    handshake_timeout: Duration::from_secs(5),
                })
                .await
                .unwrap(),
            );
            let mut status = session.status();
            while *status.borrow() != SessionStatus::Preempted {
                status.changed().await.unwrap();
            }
            let reader = {
                let session = Arc::clone(&session);
                tokio::spawn(async move { session.read_lines(Duration::from_secs(2)).await })
            };
            let started = tokio::time::Instant::now();
            assert_eq!(
                session.write(b"x", Duration::from_secs(2)).await,
                Err(SessionError::Preempted)
            );
            assert!(started.elapsed() < Duration::from_secs(1));
            let lines = reader.await.unwrap();
            assert_eq!(lines, ["after-reconnect"]);
        })
        .await;
}

#[tokio::test]
async fn at9_late_ack_after_preemption_cannot_complete_a_new_write() {
    with_watchdog(Duration::from_secs(10), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut sink, mut source) = ws.split();
                source.next().await.unwrap().unwrap(); // attach
                let attached = serde_json::json!({"type":"attached","success":true,"message":"ok","writer_pre_acquired":true});
                sink.send(tungstenite::Message::Text(attached.to_string()))
                    .await
                    .unwrap();

                source.next().await.unwrap().unwrap(); // first write
                let preempted = serde_json::json!({"type":"preempted","reason":"deploy","preempted_by":"x"});
                sink.send(tungstenite::Message::Text(preempted.to_string()))
                    .await
                    .unwrap();
                let reconnected = serde_json::json!({"type":"reconnected","message":"ok"});
                tokio::time::sleep(Duration::from_millis(20)).await;
                sink.send(tungstenite::Message::Text(reconnected.to_string()))
                    .await
                    .unwrap();

                source.next().await.unwrap().unwrap(); // second write
                for bytes_written in [1, 7] {
                    let ack = serde_json::json!({"type":"write_ack","success":true,"bytes_written":bytes_written,"message":null});
                    sink.send(tungstenite::Message::Text(ack.to_string()))
                        .await
                        .unwrap();
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            });

            let session = Arc::new(
                SerialSession::connect(SessionConfig {
                    ws_url: format!("ws://127.0.0.1:{port}"),
                    port: "COM_TEST".into(),
                    baud_rate: 115200,
                    auto_reconnect: true,
                    verbose: false,
                    client_id: "test".into(),
                    max_buffered_lines: DEFAULT_MAX_BUFFERED_LINES,
                    handshake_timeout: Duration::from_secs(5),
                })
                .await
                .unwrap(),
            );
            let first = {
                let session = Arc::clone(&session);
                tokio::spawn(async move { session.write(b"x", Duration::from_secs(2)).await })
            };
            assert_eq!(first.await.unwrap(), Err(SessionError::Preempted));
            let mut status = session.status();
            while *status.borrow() != SessionStatus::Active {
                status.changed().await.unwrap();
            }
            assert_eq!(
                session.write(b"1234567", Duration::from_secs(2)).await,
                Ok(7),
                "a late preemption-era ack must not complete a new write"
            );
        })
        .await;
}

#[tokio::test]
async fn at10_port_disconnected_wakes_everyone() {
    with_watchdog(Duration::from_secs(10), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut sink, _source) = ws.split();
                let ack = serde_json::json!({"type":"attached","success":true,"message":"ok","writer_pre_acquired":true});
                sink.send(tungstenite::Message::Text(ack.to_string()))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                let disconnected =
                    serde_json::json!({"type":"port_disconnected","port":"COM1","reason":"unplugged","message":"gone"});
                sink.send(tungstenite::Message::Text(disconnected.to_string()))
                    .await
                    .unwrap();
                std::future::pending::<()>().await;
            });
            let cfg = SessionConfig {
                ws_url: format!("ws://127.0.0.1:{port}"),
                port: "COM_TEST".into(),
                baud_rate: 115200,
                auto_reconnect: true,
                verbose: false,
                client_id: "test".into(),
                max_buffered_lines: DEFAULT_MAX_BUFFERED_LINES,
                handshake_timeout: Duration::from_secs(5),
            };
            let session = Arc::new(SerialSession::connect(cfg).await.unwrap());
            let reader = {
                let session = Arc::clone(&session);
                tokio::spawn(async move { session.read_lines(Duration::from_secs(10)).await })
            };
            let write_result = session.write(b"x", Duration::from_secs(10)).await;
            assert_eq!(write_result, Err(SessionError::PortGone));
            let lines = tokio::time::timeout(Duration::from_secs(2), reader)
                .await
                .unwrap()
                .unwrap();
            assert!(lines.is_empty());
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(!session.is_reader_alive(), "reader task must have finished");
        })
        .await;
}

#[path = "tests/extended.rs"]
mod extended;

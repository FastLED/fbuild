use super::*;
#[tokio::test]
async fn at11_overflow_drops_oldest_and_counts() {
    with_watchdog(Duration::from_secs(30), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(fake_daemon(
            listener,
            DaemonKnobs {
                bulk_lines_on_attach: 50_000,
                suppress_write_echo: true,
                ..Default::default()
            },
        ));
        let cfg = SessionConfig {
            ws_url: format!("ws://127.0.0.1:{port}"),
            port: "COM_TEST".into(),
            baud_rate: 115200,
            auto_reconnect: true,
            verbose: false,
            client_id: "test".into(),
            max_buffered_lines: 10_000,
            handshake_timeout: Duration::from_secs(5),
        };
        let session = SerialSession::connect(cfg).await.unwrap();
        for i in 0..10 {
            let payload = format!("m{i}");
            let n = session
                .write(payload.as_bytes(), Duration::from_secs(2))
                .await
                .unwrap();
            assert_eq!(n, payload.len(), "write {i} stalled during overflow");
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while session.lines_dropped() < 40_000 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(session.lines_dropped(), 40_000);
        let remaining = session.read_lines(Duration::from_millis(50)).await;
        assert_eq!(remaining.len(), 10_000);
        assert_eq!(remaining.first().unwrap(), "line-40000");
        assert_eq!(remaining.last().unwrap(), "line-49999");
        // The socket kept draining: a write after the overflow still acks.
        let n = session.write(b"zz", Duration::from_secs(2)).await.unwrap();
        assert_eq!(n, 2);
    })
    .await;
}

#[tokio::test]
async fn at12_clear_input_registers_no_reply() {
    with_watchdog(Duration::from_secs(10), async {
        let (session, _) = connect_to(DaemonKnobs::default()).await;
        session.write(b"a", Duration::from_secs(2)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        session.clear_input().await;
        assert!(
            session
                .read_lines(Duration::from_millis(50))
                .await
                .is_empty()
        );
        let n = session.write(b"bb", Duration::from_secs(2)).await.unwrap();
        assert_eq!(n, 2, "clear_input must not consume the next write's ack");
    })
    .await;
}

#[tokio::test]
async fn at14_abandoned_reader_does_not_eat_the_reply() {
    with_watchdog(Duration::from_secs(30), async {
        let (session, _) = connect_to(DaemonKnobs::default()).await;
        let session = Arc::new(session);
        for _ in 0..100 {
            let a = {
                let session = Arc::clone(&session);
                tokio::spawn(async move { session.read_lines(Duration::from_secs(10)).await })
            };
            tokio::time::sleep(Duration::from_millis(2)).await;
            a.abort(); // abandon reader A: cancellation removes nothing
            let n = session
                .write(b"z", Duration::from_millis(500))
                .await
                .unwrap();
            assert_eq!(n, 1);
            let b = session.read_lines(Duration::from_millis(500)).await;
            assert!(b.contains(&"echo:z".to_string()));
        }
    })
    .await;
}

#[tokio::test]
async fn at14_interrupt_wakes_blocked_reader_without_draining_next_reply() {
    with_watchdog(Duration::from_secs(10), async {
        let (session, _) = connect_to(DaemonKnobs::default()).await;
        let session = Arc::new(session);
        for i in 0..100 {
            let reader = {
                let session = Arc::clone(&session);
                tokio::spawn(async move { session.read_lines(Duration::from_secs(10)).await })
            };
            tokio::time::sleep(Duration::from_millis(1)).await;
            session.interrupt_reads();
            let abandoned = tokio::time::timeout(Duration::from_millis(100), reader)
                .await
                .expect("interrupted reader remained blocked")
                .expect("reader task panicked");
            assert!(abandoned.is_empty(), "interrupted reader drained lines");
            let payload = format!("request-{i}");
            session
                .write(payload.as_bytes(), Duration::from_secs(1))
                .await
                .unwrap();
            let lines = session.read_lines(Duration::from_secs(1)).await;
            assert_eq!(lines, vec![format!("echo:{payload}")]);
        }
    })
    .await;
}

#[tokio::test]
async fn at15_peer_stops_reading_write_times_out_and_closes() {
    with_watchdog(Duration::from_secs(10), async {
        let (session, _) = connect_to(DaemonKnobs {
            stop_reading_after_writes: Some(0),
            ..Default::default()
        })
        .await;
        let session = Arc::new(session);
        let reader = {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.read_lines(Duration::from_secs(5)).await })
        };
        let payload = vec![b'x'; 8 * 1024 * 1024];
        let started = tokio::time::Instant::now();
        let result = session.write(&payload, Duration::from_millis(300)).await;
        assert_eq!(result, Err(SessionError::Timeout));
        assert!(started.elapsed() < Duration::from_millis(1300));
        assert_eq!(session.inner.status(), SessionStatus::Closed);
        assert!(
            session.inner.sink.try_lock().is_ok(),
            "timed-out send retained the sink lock"
        );
        let lines = tokio::time::timeout(Duration::from_secs(1), reader)
            .await
            .expect("reader did not wake after send timeout")
            .unwrap();
        assert!(lines.is_empty());
        assert_eq!(
            session.write(b"later", Duration::from_secs(1)).await,
            Err(SessionError::Closed)
        );
    })
    .await;
}

#[tokio::test]
async fn at16_handshake_stall_times_out() {
    with_watchdog(Duration::from_secs(10), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // Accept the TCP connection but never complete the WS
            // handshake.
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await
        });
        let cfg = SessionConfig {
            ws_url: format!("ws://127.0.0.1:{port}"),
            port: "COM_TEST".into(),
            baud_rate: 115200,
            auto_reconnect: true,
            verbose: false,
            client_id: "test".into(),
            max_buffered_lines: DEFAULT_MAX_BUFFERED_LINES,
            handshake_timeout: Duration::from_millis(300),
        };
        let started = tokio::time::Instant::now();
        let result = SerialSession::connect(cfg).await;
        assert_eq!(result.err(), Some(SessionError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(5));
    })
    .await;
}

#[tokio::test]
async fn at17_reader_task_panic_wakes_pending_calls() {
    with_watchdog(Duration::from_secs(10), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut sink, _source) = ws.split();
                let attached = serde_json::json!({"type":"attached","success":true,"message":"ok","writer_pre_acquired":true});
                sink.send(tungstenite::Message::Text(attached.to_string()))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                sink.send(tungstenite::Message::Text("__fbuild_test_reader_panic__".into()))
                    .await
                    .unwrap();
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
            let reader = {
                let session = Arc::clone(&session);
                tokio::spawn(async move { session.read_lines(Duration::from_secs(5)).await })
            };
            let writer = {
                let session = Arc::clone(&session);
                tokio::spawn(async move { session.write(b"x", Duration::from_secs(5)).await })
            };
            let rpc = {
                let session = Arc::clone(&session);
                tokio::spawn(async move {
                    session
                        .json_rpc("REMOTE:{\"id\":1}", Duration::from_secs(5))
                        .await
                })
            };
            assert!(reader.await.unwrap().is_empty());
            assert_eq!(writer.await.unwrap(), Err(SessionError::Closed));
            assert_eq!(rpc.await.unwrap(), Err(SessionError::Closed));
            assert_eq!(
                session.write(b"later", Duration::from_secs(1)).await,
                Err(SessionError::Closed)
            );
            assert!(!session.is_reader_alive());
        })
        .await;
}

#[tokio::test]
async fn at18_malformed_frames_are_ignored() {
    with_watchdog(Duration::from_secs(10), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut sink, mut source) = ws.split();
                let ack = serde_json::json!({"type":"attached","success":true,"message":"ok","writer_pre_acquired":true});
                sink.send(tungstenite::Message::Text(ack.to_string()))
                    .await
                    .unwrap();
                sink.send(tungstenite::Message::Text("not json".into()))
                    .await
                    .unwrap();
                sink.send(tungstenite::Message::Text(r#"{"type":"unknown_thing"}"#.into()))
                    .await
                    .unwrap();
                sink.send(tungstenite::Message::Binary(vec![1, 2, 3].into()))
                    .await
                    .unwrap();
                while let Some(Ok(tungstenite::Message::Text(text))) = source.next().await {
                    let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if req["type"] == "write" {
                        let data = req["data"].as_str().unwrap();
                        let decoded = base64::Engine::decode(
                            &base64::engine::general_purpose::STANDARD,
                            data.as_bytes(),
                        )
                        .unwrap();
                        let reply = serde_json::json!({"type":"write_ack","success":true,"bytes_written":decoded.len(),"message":null});
                        sink.send(tungstenite::Message::Text(reply.to_string()))
                            .await
                            .unwrap();
                    }
                }
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
            let session = SerialSession::connect(cfg).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let n = session.write(b"ok", Duration::from_secs(2)).await.unwrap();
            assert_eq!(n, 2, "session must keep working after malformed frames");
        })
        .await;
}

/// AT-20: `close()` while 16 calls are in flight, 100 iterations. Every
/// in-flight call must resolve within 1s of the close (never hang until
/// its own longer timeout), and the reader task must be joined
/// (`JoinHandle::is_finished()`) by the time `close()` returns.
#[tokio::test]
async fn at20_teardown_races_in_flight_calls() {
    with_watchdog(Duration::from_secs(60), async {
            for iteration in 0..100 {
                let (session, _) = connect_to(DaemonKnobs {
                    ack_delay: Duration::from_secs(2),
                    ..Default::default()
                })
                .await;
                let session = Arc::new(session);
                let mut handles = Vec::new();
                for i in 0..16 {
                    let session = Arc::clone(&session);
                    handles.push(tokio::spawn(async move {
                        if i % 2 == 0 {
                            session.write(b"x", Duration::from_secs(5)).await.err()
                        } else {
                            // read_lines never errors; treat an empty batch
                            // as "closed while waiting" for this check.
                            let lines = session.read_lines(Duration::from_secs(5)).await;
                            if lines.is_empty() {
                                Some(SessionError::Closed)
                            } else {
                                None
                            }
                        }
                    }));
                }
                // Let calls actually get in flight before closing.
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(2)).await;

                let started = tokio::time::Instant::now();
                session.close().await;
                assert!(
                    !session.is_reader_alive(),
                    "iteration {iteration}: reader task must be joined by the time close() returns"
                );

                let all = futures::future::join_all(handles);
                let results = tokio::time::timeout(Duration::from_secs(1), all)
                    .await
                    .unwrap_or_else(|_| {
                        panic!(
                            "iteration {iteration}: in-flight calls did not return within 1s of close() (took >{:?})",
                            started.elapsed()
                        )
                    });
                for result in results {
                    assert_eq!(
                        result.expect("in-flight task panicked"),
                        Some(SessionError::Closed),
                        "iteration {iteration}: in-flight call did not observe close"
                    );
                }
            }
        })
        .await;
}

/// AT-13: throughput sanity. 10,000 request/reply round trips on
/// loopback; median latency under 5ms and no desync warnings.
#[tokio::test]
async fn at13_throughput_sanity() {
    with_watchdog(Duration::from_secs(60), async {
        #[derive(Clone)]
        struct WarningWriter(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for WarningWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_writer({
                let warnings = Arc::clone(&warnings);
                move || WarningWriter(Arc::clone(&warnings))
            })
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        let (session, _) = connect_to(DaemonKnobs::default()).await;
        let mut latencies = Vec::with_capacity(10_000);
        for i in 0..10_000u32 {
            let started = tokio::time::Instant::now();
            let payload = format!("m{i}");
            let n = session
                .write(payload.as_bytes(), Duration::from_secs(5))
                .await
                .unwrap();
            assert_eq!(n, payload.len());
            latencies.push(started.elapsed());
        }
        latencies.sort();
        let median = latencies[latencies.len() / 2];
        assert!(
            median < Duration::from_millis(5),
            "median round-trip latency {median:?} exceeded 5ms over {} requests",
            latencies.len()
        );
        let captured = String::from_utf8(warnings.lock().unwrap().clone()).unwrap();
        assert!(
            !captured.contains("protocol desync"),
            "throughput run emitted a protocol-desync warning: {captured}"
        );
    })
    .await;
}

/// AT-19: chaos soak. 64 tasks run a random mix of `read_lines`,
/// `write`, `json_rpc`, `in_waiting`, `clear_input` and cancellations
/// for the full 30s acceptance window, against a daemon
/// with seeded random delays, injected errors, a preemption/reconnect
/// pair, and a timed port disconnect followed by a fresh connection. No call
/// may exceed its own timeout + 1s; the reader task is joined at the
/// end. Seeded so a failure is reproducible; the seed is printed
/// unconditionally (not just on failure) so a CI log always carries it.
#[tokio::test]
async fn at19_chaos_soak() {
    use futures::FutureExt;
    let seed: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    println!("at19_chaos_soak seed = {seed}");
    let result = std::panic::AssertUnwindSafe(run_at19_chaos_soak(seed))
        .catch_unwind()
        .await;
    if let Err(e) = result {
        eprintln!("at19_chaos_soak FAILED with seed = {seed}");
        std::panic::resume_unwind(e);
    }
}

async fn run_at19_chaos_soak(seed: u64) {
    with_watchdog(Duration::from_secs(90), async {
            let (session, _) = connect_to(DaemonKnobs {
                echo_delay: Duration::from_millis(5),
                chaos_seed: Some(seed),
                preempt_on_nth_write: Some(17),
                disconnect_after: Some(Duration::from_secs(12)),
                ..Default::default()
            })
            .await;
            let first_session = Arc::new(session);
            let current_session = Arc::new(tokio::sync::RwLock::new(Arc::clone(&first_session)));
            let replacement_ops = Arc::new(AtomicUsize::new(0));
            let soak_budget = Duration::from_secs(30);
            let deadline = tokio::time::Instant::now() + soak_budget;

            let swap_session = {
                let current_session = Arc::clone(&current_session);
                let first_session = Arc::clone(&first_session);
                tokio::spawn(async move {
                    let mut status = first_session.status();
                    tokio::time::timeout(Duration::from_secs(17), async {
                        while *status.borrow() != SessionStatus::Closed {
                            status.changed().await.expect("session status sender dropped");
                        }
                    })
                    .await
                    .expect("fake daemon never disconnected the first session");
                    let (replacement, _) = connect_to(DaemonKnobs {
                        echo_delay: Duration::from_millis(5),
                        chaos_seed: Some(seed ^ 0xA5A5_5A5A),
                        preempt_on_nth_write: Some(17),
                        ..Default::default()
                    })
                    .await;
                    let replacement = Arc::new(replacement);
                    *current_session.write().await = Arc::clone(&replacement);
                    first_session.close().await;
                    assert!(
                        !first_session.is_reader_alive(),
                        "disconnected reader task did not terminate (seed={seed})"
                    );
                    assert_line_accounting(&first_session, seed, "disconnected");
                    replacement
                })
            };

            let mut handles = Vec::new();
            for task_id in 0..64u64 {
                let current_session = Arc::clone(&current_session);
                let first_session = Arc::clone(&first_session);
                let replacement_ops = Arc::clone(&replacement_ops);
                let mut rng = Xorshift64::new(seed ^ task_id.wrapping_mul(0x9E3779B97F4A7C15));
                handles.push(tokio::spawn(async move {
                    let mut iterations = 0u64;
                    while tokio::time::Instant::now() < deadline {
                        let session = Arc::clone(&*current_session.read().await);
                        if !Arc::ptr_eq(&session, &first_session) {
                            replacement_ops.fetch_add(1, Ordering::Relaxed);
                        }
                        iterations += 1;
                        let op = rng.below(5);
                        let per_call_timeout = Duration::from_millis(200 + rng.below(300));
                        let call_started = tokio::time::Instant::now();
                        let watchdog = per_call_timeout + Duration::from_secs(1);
                        let outcome = tokio::time::timeout(watchdog, async {
                            match op {
                                0 => {
                                    let _ = session.read_lines(per_call_timeout).await;
                                }
                                1 => {
                                    let payload = format!("t{task_id}-{iterations}");
                                    match session.write(payload.as_bytes(), per_call_timeout).await {
                                        Ok(bytes_written) => assert_eq!(
                                            bytes_written,
                                            payload.len(),
                                            "task {task_id} received another write's ack (seed={seed})"
                                        ),
                                        Err(SessionError::Timeout
                                        | SessionError::Closed
                                        | SessionError::ConnectionFailed(_)
                                        | SessionError::PortGone
                                        | SessionError::Preempted
                                        | SessionError::ProtocolDesync
                                        | SessionError::WriteFailed(_)) => {}
                                    }
                                }
                                2 => {
                                    let _ = session
                                        .json_rpc(
                                            &format!("REMOTE:{{\"t\":{task_id}}}"),
                                            per_call_timeout,
                                        )
                                        .await;
                                }
                                3 => {
                                    let _ = session.in_waiting(per_call_timeout).await;
                                }
                                _ => {
                                    session.clear_input().await;
                                }
                            }
                        })
                        .await;
                        assert!(
                            outcome.is_ok(),
                            "task {task_id} op {op} exceeded its own timeout ({per_call_timeout:?}) + 1s (seed={seed}, elapsed={:?})",
                            call_started.elapsed()
                        );
                        // Cancellation mix: occasionally abandon a read
                        // mid-flight instead of awaiting it to completion.
                        if op == 0 && rng.below(4) == 0 {
                            let fut = session.read_lines(per_call_timeout);
                            let _ = tokio::time::timeout(Duration::from_millis(1), fut).await;
                        }
                    }
                }));
            }

            for h in handles {
                h.await.expect("chaos task panicked");
            }
            let session = swap_session.await.expect("reconnect task panicked");
            assert!(
                replacement_ops.load(Ordering::Relaxed) > 0,
                "workers never used the replacement session (seed={seed})"
            );
            assert!(
                session.is_reader_alive(),
                "replacement reader task must still be alive at the end of the soak (seed={seed})"
            );
            session.close().await;
            assert!(
                !session.inner.reader_alive.load(Ordering::Relaxed),
                "reader task did not terminate after close (seed={seed})"
            );
            assert_line_accounting(&session, seed, "replacement");
        })
        .await;
}
#[tokio::test]
async fn refused_daemon_connection_keeps_fastled_recovery_hint() {
    with_watchdog(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let cfg = SessionConfig {
            ws_url: format!("ws://127.0.0.1:{port}/ws/serial-monitor"),
            port: "COM_TEST".into(),
            baud_rate: 115200,
            auto_reconnect: true,
            verbose: false,
            client_id: "test".into(),
            max_buffered_lines: DEFAULT_MAX_BUFFERED_LINES,
            handshake_timeout: Duration::from_secs(1),
        };
        let error = SerialSession::connect(cfg)
            .await
            .err()
            .expect("connection should fail");
        assert!(error.to_string().contains("daemon WebSocket"), "{error}");
    })
    .await;
}

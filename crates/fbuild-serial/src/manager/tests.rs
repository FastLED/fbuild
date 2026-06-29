//! Unit tests for the parent `manager` module. Extracted to keep the
//! parent file under the 1000-LOC gate (see ci.yml LOC Gate workflow).

use super::*;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, StopBits};
use std::io::{Read, Write};

#[derive(Clone)]
struct FakeSerialPort {
    name: String,
    writes: Arc<std::sync::Mutex<Vec<u8>>>,
}

impl FakeSerialPort {
    fn new(name: &str) -> (Self, Arc<std::sync::Mutex<Vec<u8>>>) {
        let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                name: name.to_string(),
                writes: Arc::clone(&writes),
            },
            writes,
        )
    }
}

impl Read for FakeSerialPort {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no data"))
    }
}

impl Write for FakeSerialPort {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writes.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl serialport::SerialPort for FakeSerialPort {
    fn name(&self) -> Option<String> {
        Some(self.name.clone())
    }
    fn baud_rate(&self) -> serialport::Result<u32> {
        Ok(115200)
    }
    fn data_bits(&self) -> serialport::Result<DataBits> {
        Ok(DataBits::Eight)
    }
    fn flow_control(&self) -> serialport::Result<FlowControl> {
        Ok(FlowControl::None)
    }
    fn parity(&self) -> serialport::Result<Parity> {
        Ok(Parity::None)
    }
    fn stop_bits(&self) -> serialport::Result<StopBits> {
        Ok(StopBits::One)
    }
    fn timeout(&self) -> Duration {
        Duration::from_millis(100)
    }
    fn set_baud_rate(&mut self, _baud_rate: u32) -> serialport::Result<()> {
        Ok(())
    }
    fn set_data_bits(&mut self, _data_bits: DataBits) -> serialport::Result<()> {
        Ok(())
    }
    fn set_flow_control(&mut self, _flow_control: FlowControl) -> serialport::Result<()> {
        Ok(())
    }
    fn set_parity(&mut self, _parity: Parity) -> serialport::Result<()> {
        Ok(())
    }
    fn set_stop_bits(&mut self, _stop_bits: StopBits) -> serialport::Result<()> {
        Ok(())
    }
    fn set_timeout(&mut self, _timeout: Duration) -> serialport::Result<()> {
        Ok(())
    }
    fn write_request_to_send(&mut self, _level: bool) -> serialport::Result<()> {
        Ok(())
    }
    fn write_data_terminal_ready(&mut self, _level: bool) -> serialport::Result<()> {
        Ok(())
    }
    fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
        Ok(true)
    }
    fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
        Ok(true)
    }
    fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
        Ok(false)
    }
    fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
        Ok(true)
    }
    fn bytes_to_read(&self) -> serialport::Result<u32> {
        Ok(0)
    }
    fn bytes_to_write(&self) -> serialport::Result<u32> {
        Ok(0)
    }
    fn clear(&self, _buffer_to_clear: ClearBuffer) -> serialport::Result<()> {
        Ok(())
    }
    fn try_clone(&self) -> serialport::Result<Box<dyn serialport::SerialPort>> {
        Ok(Box::new(self.clone()))
    }
    fn set_break(&self) -> serialport::Result<()> {
        Ok(())
    }
    fn clear_break(&self) -> serialport::Result<()> {
        Ok(())
    }
}

/// TDD red→green for ISSUES.md "Issue C": calling `open_port` against a
/// definitely-nonexistent port must NOT block other tokio tasks on the
/// same multi-thread runtime. Before the spawn_blocking fix, the
/// synchronous `serialport::open()` call held one of the worker threads
/// for the full retry budget; with only one worker, a concurrently
/// scheduled task could not run until the open finished.
///
/// We use a 1-worker multi-thread runtime to make the regression
/// observable: with the fix, the keepalive task runs while the open
/// retries are blocked on a *blocking-pool* thread; without the fix,
/// the open call hogs the only worker and the keepalive ticks never
/// fire until the retries time out (~15s on Windows).
#[test]
fn open_port_does_not_starve_runtime_workers() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        let mgr = Arc::new(SharedSerialManager::new());
        let mgr_open = Arc::clone(&mgr);

        // Pick a port name that cannot exist on any platform so the
        // open call always fails and retries through the full schedule.
        // Using a very long invalid name avoids the slim chance of
        // matching an actual /dev/ttyUSB* on Linux CI runners.
        let bogus_port = "FBUILD_TEST_NONEXISTENT_PORT_xyz_zzz".to_string();

        let open_task = tokio::spawn(async move {
            let _ = mgr_open
                .open_port(&bogus_port, 115200, "test_client", None)
                .await;
        });

        // Concurrent keepalive: should tick at least 5 times (5 × 50ms
        // = 250ms) within the first second of the open retries. With
        // the bug present, this counter would still be 0 because the
        // single worker is blocked inside `serialport::open()`.
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ticks_clone = Arc::clone(&ticks);
        let keepalive = tokio::spawn(async move {
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                ticks_clone.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Wait for the keepalive to finish (1s).
        let _ = tokio::time::timeout(Duration::from_secs(3), keepalive).await;
        let observed = ticks.load(Ordering::Relaxed);

        // Abort the open task — we don't care about its result, only
        // that it didn't starve the runtime.
        open_task.abort();

        assert!(
            observed >= 5,
            "concurrent task ticked {} times in 1s while open_port \
             was retrying — runtime worker is starved (Issue C \
             regression: serialport::open() must run via spawn_blocking)",
            observed
        );
    });
}

/// Regression guard for FastLED/fbuild#51: `attach_reader` used to
/// insert `client_id` into `session.reader_client_ids` even when the
/// broadcaster was missing (returning `None`). That left a dangling
/// reader id that kept `has_clients()` true forever, blocking
/// self-eviction and leaving `fbuild-daemon.exe` resident after the
/// autoresearch session ended.
///
/// Contract: if `attach_reader` returns `None`, no session state may
/// be mutated.
#[test]
fn attach_reader_missing_broadcaster_does_not_mutate_session_state() {
    let mgr = SharedSerialManager::new();
    let port = "COM_TEST_NO_BROADCASTER";
    let client = "client-1";

    // Insert a bare session without a broadcaster — simulates the
    // pathological "half-set-up" state.
    mgr.sessions.insert(
        port.to_string(),
        super::SerialSession {
            port: port.to_string(),
            baud_rate: 115200,
            is_open: false,
            writer_client_id: None,
            reader_client_ids: Default::default(),
            output_buffer: Default::default(),
            total_bytes_read: 0,
            total_bytes_written: 0,
            started_at: 0.0,
            owner_client_id: None,
            elf_path: None,
            serial_handle: None,
            reader_handle: None,
            stop_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );

    let result = mgr.attach_reader(port, client);
    assert!(
        result.is_none(),
        "attach_reader must return None when broadcaster is absent"
    );

    let leaked = mgr
        .sessions
        .get(port)
        .map(|s| s.reader_client_ids.contains(client))
        .unwrap_or(false);
    assert!(
        !leaked,
        "attach_reader must not mutate reader_client_ids when it \
         returns None — regression of FastLED/fbuild#51 where the \
         leaked id kept has_clients() true forever"
    );
}

/// Regression guard for FastLED/fbuild#531: after a timeout/halt monitor
/// session ends, the HTTP `monitor` handler detaches its reader and then
/// closes the port when no clients remain, releasing the OS serial handle
/// so a follow-up pyserial/esptool open of the same port succeeds without
/// `fbuild daemon stop`. This locks in the manager contract that the
/// handler relies on: detach drops the last client, and close removes the
/// session (and its serial handle) from the manager entirely.
#[tokio::test]
async fn detach_then_close_releases_port_for_lone_monitor() {
    let mgr = SharedSerialManager::new();
    let port = "COM_TEST_531";
    let client = "monitor-client";

    // Simulate an open, single-reader monitor session (the timeout path):
    // a broadcaster is present and one reader is attached.
    let (tx, _rx) = broadcast::channel(BROADCAST_CHANNEL_SIZE);
    mgr.broadcasters.insert(port.to_string(), tx);
    let mut readers = std::collections::HashSet::new();
    readers.insert(client.to_string());
    mgr.sessions.insert(
        port.to_string(),
        super::SerialSession {
            port: port.to_string(),
            baud_rate: 115200,
            is_open: true,
            writer_client_id: None,
            reader_client_ids: readers,
            output_buffer: Default::default(),
            total_bytes_read: 0,
            total_bytes_written: 0,
            started_at: 0.0,
            owner_client_id: Some(client.to_string()),
            elf_path: None,
            serial_handle: None,
            reader_handle: None,
            stop_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );

    assert!(
        mgr.has_clients(port),
        "precondition: the lone monitor reader keeps the port busy"
    );

    // Mirror the handler cleanup sequence.
    mgr.detach_reader(port, client);
    assert!(
        !mgr.has_clients(port),
        "after the lone monitor detaches, no clients should remain"
    );
    mgr.close_port(port, client).await.expect("close_port");

    // The session (and its serial handle) must be gone — the OS port is
    // released, so a non-fbuild client can reopen it.
    assert!(
        mgr.sessions.get(port).is_none(),
        "close_port must remove the session so the OS handle is released \
         (regression of FastLED/fbuild#531)"
    );
    assert!(
        mgr.broadcasters.get(port).is_none(),
        "close_port must also drop the broadcaster for the released port"
    );
}

#[tokio::test]
async fn grace_close_removes_idle_port_after_delay() {
    let mgr = Arc::new(SharedSerialManager::new());
    let port = "COM_TEST_GRACE_CLOSE";
    let client = "monitor-client";

    mgr.sessions.insert(
        port.to_string(),
        super::SerialSession {
            port: port.to_string(),
            baud_rate: 115200,
            is_open: true,
            writer_client_id: None,
            reader_client_ids: Default::default(),
            output_buffer: Default::default(),
            total_bytes_read: 0,
            total_bytes_written: 0,
            started_at: 0.0,
            owner_client_id: Some(client.to_string()),
            elf_path: None,
            serial_handle: None,
            reader_handle: None,
            stop_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );

    assert!(mgr.close_port_after_grace_if_idle(port, client, Duration::from_millis(10)));
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        mgr.sessions.get(port).is_none(),
        "idle grace close should physically release the port after delay"
    );
}

#[tokio::test]
async fn grace_close_is_canceled_by_new_reader() {
    let mgr = Arc::new(SharedSerialManager::new());
    let port = "COM_TEST_GRACE_CANCEL";
    let client = "monitor-client";
    let next_client = "next-client";

    let (tx, _rx) = broadcast::channel(BROADCAST_CHANNEL_SIZE);
    mgr.broadcasters.insert(port.to_string(), tx);
    mgr.sessions.insert(
        port.to_string(),
        super::SerialSession {
            port: port.to_string(),
            baud_rate: 115200,
            is_open: true,
            writer_client_id: None,
            reader_client_ids: Default::default(),
            output_buffer: Default::default(),
            total_bytes_read: 0,
            total_bytes_written: 0,
            started_at: 0.0,
            owner_client_id: Some(client.to_string()),
            elf_path: None,
            serial_handle: None,
            reader_handle: None,
            stop_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );

    assert!(mgr.close_port_after_grace_if_idle(port, client, Duration::from_millis(25)));
    let rx = mgr.attach_reader(port, next_client);
    assert!(
        rx.is_some(),
        "new reader should attach during pending close grace window"
    );

    tokio::time::sleep(Duration::from_millis(75)).await;

    assert!(
        mgr.sessions.get(port).is_some(),
        "new reader activity should cancel the pending physical close"
    );
    assert!(mgr.has_clients(port));
}

#[tokio::test]
async fn stale_deploy_preemption_close_preserves_new_monitor_session() {
    let mgr = SharedSerialManager::new();
    let port = "COM_TEST_811";
    let original_client = "deploy-preempted-client";
    let monitor_client = "post-deploy-monitor";

    let (tx, _rx) = broadcast::channel(BROADCAST_CHANNEL_SIZE);
    mgr.broadcasters.insert(port.to_string(), tx);
    let (fake, writes) = FakeSerialPort::new(port);
    mgr.sessions.insert(
        port.to_string(),
        SerialSession {
            port: port.to_string(),
            baud_rate: 115200,
            is_open: true,
            writer_client_id: None,
            reader_client_ids: Default::default(),
            output_buffer: Default::default(),
            total_bytes_read: 0,
            total_bytes_written: 0,
            started_at: 0.0,
            owner_client_id: Some(original_client.to_string()),
            elf_path: None,
            serial_handle: Some(Arc::new(Mutex::new(Box::new(fake)))),
            reader_handle: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        },
    );

    // A deploy preemption can capture the old session generation, yield,
    // then resume after the post-deploy monitor has attached. The stale
    // close must not remove the new monitor session or later writes fail
    // with "port not open" (FastLED/fbuild#811).
    let stale_generation = mgr.bump_close_generation(port);
    assert!(mgr.attach_reader(port, monitor_client).is_some());
    assert_ne!(mgr.close_generation(port), Some(stale_generation));

    let closed = mgr
        .close_port_if_generation(port, "deploy_preemption", Some(stale_generation))
        .await
        .expect("generation-checked close");
    assert!(
        !closed,
        "stale deploy preemption close must not remove the active monitor"
    );

    mgr.acquire_writer(port, monitor_client)
        .await
        .expect("writer lock");
    let payload = b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n";
    let bytes = mgr
        .write_to_port(port, payload, monitor_client)
        .await
        .expect("write to active monitor session");

    assert_eq!(bytes, payload.len());
    assert_eq!(&*writes.lock().unwrap(), payload);
}

#[test]
fn notify_port_renumbered_broadcasts_events_to_old_port() {
    let mgr = SharedSerialManager::new();
    let old_port = "COM21";
    let new_port = "COM20";
    let (tx, mut rx) = broadcast::channel(BROADCAST_CHANNEL_SIZE);
    mgr.broadcasters.insert(old_port.to_string(), tx);

    assert!(mgr.notify_port_renumbered(
        old_port,
        new_port,
        "tracked_serial_move",
        Some("15821020".to_string())
    ));

    assert_eq!(
        rx.try_recv().unwrap(),
        SerialStreamEvent::PortRenumbered {
            port: old_port.to_string(),
            new_port: new_port.to_string(),
            reason: "tracked_serial_move".to_string(),
            serial: Some("15821020".to_string()),
        }
    );
    assert_eq!(
        rx.try_recv().unwrap(),
        SerialStreamEvent::PortReattached {
            port: new_port.to_string(),
            previous_port: old_port.to_string(),
        }
    );
}

#[tokio::test]
async fn rebind_preserves_session_and_routes_writes_to_new_handle() {
    let mgr = SharedSerialManager::new();
    let old_port = "COM21";
    let new_port = "COM20";
    let writer = "writer-client";
    let reader = "reader-client";
    let (tx, mut rx) = broadcast::channel(BROADCAST_CHANNEL_SIZE);
    mgr.broadcasters.insert(old_port.to_string(), tx);
    mgr.output_buffers.insert(
        old_port.to_string(),
        Arc::new(PortOutputBuffer {
            buffer: std::sync::Mutex::new(VecDeque::with_capacity(OUTPUT_BUFFER_CAP)),
            total_bytes_read: std::sync::atomic::AtomicU64::new(0),
        }),
    );
    let (old_fake, _old_writes) = FakeSerialPort::new(old_port);
    let mut readers = std::collections::HashSet::new();
    readers.insert(reader.to_string());
    mgr.sessions.insert(
        old_port.to_string(),
        SerialSession {
            port: old_port.to_string(),
            baud_rate: 115200,
            is_open: true,
            writer_client_id: Some(writer.to_string()),
            reader_client_ids: readers,
            output_buffer: Default::default(),
            total_bytes_read: 0,
            total_bytes_written: 0,
            started_at: 0.0,
            owner_client_id: Some(writer.to_string()),
            elf_path: None,
            serial_handle: Some(Arc::new(Mutex::new(Box::new(old_fake)))),
            reader_handle: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        },
    );
    let (new_fake, new_writes) = FakeSerialPort::new(new_port);

    assert!(mgr
        .rebind_port_session_to_handle(
            old_port,
            new_port,
            Arc::new(Mutex::new(Box::new(new_fake))),
            "tracked_serial_move",
            Some("15821020".to_string()),
        )
        .await
        .unwrap());

    let session = mgr.sessions.get(old_port).expect("logical session remains");
    assert_eq!(session.port, new_port);
    assert_eq!(session.writer_client_id.as_deref(), Some(writer));
    assert!(session.reader_client_ids.contains(reader));
    drop(session);
    assert_eq!(mgr.reader_count(new_port), 1);
    assert!(mgr.has_clients(new_port));

    mgr.write_to_port(old_port, b"old-logical", writer)
        .await
        .unwrap();
    mgr.write_to_port(new_port, b"new-alias", writer)
        .await
        .unwrap();
    assert_eq!(&*new_writes.lock().unwrap(), b"old-logicalnew-alias");

    assert_eq!(
        rx.try_recv().unwrap(),
        SerialStreamEvent::PortRenumbered {
            port: old_port.to_string(),
            new_port: new_port.to_string(),
            reason: "tracked_serial_move".to_string(),
            serial: Some("15821020".to_string()),
        }
    );
    assert_eq!(
        rx.try_recv().unwrap(),
        SerialStreamEvent::PortReattached {
            port: new_port.to_string(),
            previous_port: old_port.to_string(),
        }
    );

    mgr.close_port(new_port, "test").await.unwrap();
    assert!(mgr.sessions.get(old_port).is_none());
    assert!(mgr.port_aliases.get(new_port).is_none());
}

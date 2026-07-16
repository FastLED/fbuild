//! SharedSerialManager: centralized serial port access. All serial I/O
//! flows through this single manager in the daemon. See
//! `docs/architecture/serial.md` for the concurrency model
//! (per-port `tokio::sync::Mutex`, per-port reader task, broadcast for
//! readers, exclusive writer) and the Windows USB-CDC write strategy.

use crate::crash_decoder::CrashDecoder;
use crate::messages::{SerialClientMetadata, SerialStreamEvent};
use crate::preemption::PreemptionTracker;
use crate::session::SerialSession;
use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, broadcast};

const OUTPUT_BUFFER_CAP: usize = 10_000;
const BROADCAST_CHANNEL_SIZE: usize = 1024;
const READ_BUF_SIZE: usize = 4096;

fn now_unix_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn millis_to_unix_secs(ms: u64) -> Option<f64> {
    (ms > 0).then(|| ms as f64 / 1000.0)
}

/// Per-port output buffer shared with the background reader. Separate from
/// `SerialSession` so we don't need `SerialSession: Clone`.
struct PortOutputBuffer {
    buffer: std::sync::Mutex<VecDeque<String>>,
    total_bytes_read: std::sync::atomic::AtomicU64,
    last_read_at_ms: std::sync::atomic::AtomicU64,
}

/// Central serial port manager. One instance per daemon.
pub struct SharedSerialManager {
    sessions: DashMap<String, SerialSession>,
    /// Alias from an OS port observed after USB renumbering back to the
    /// logical session key that existing clients attached to.
    port_aliases: DashMap<String, String>,
    /// Broadcast channels per port for output distribution.
    broadcasters: DashMap<String, broadcast::Sender<SerialStreamEvent>>,
    /// Monotonic per-port generation that invalidates delayed physical closes.
    close_generations: DashMap<String, u64>,
    preemption: Arc<PreemptionTracker>,
    /// Per-port crash decoders for translating crash addresses to source locations.
    crash_decoders: DashMap<String, CrashDecoder>,
    /// Per-port output buffers shared with background reader threads.
    output_buffers: DashMap<String, Arc<PortOutputBuffer>>,
}

impl SharedSerialManager {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            port_aliases: DashMap::new(),
            broadcasters: DashMap::new(),
            close_generations: DashMap::new(),
            preemption: Arc::new(PreemptionTracker::new()),
            crash_decoders: DashMap::new(),
            output_buffers: DashMap::new(),
        }
    }

    /// Open a serial port. Retries with backoff for Windows USB-CDC.
    ///
    /// `family` (FastLED/fbuild#687) consults
    /// [`crate::boards::BoardFamily::idle_dtr_rts`] for the post-open
    /// DTR/RTS state. `None` falls back to the safe default of
    /// `(true, true)` — works for every CDC-ACM bridge plus
    /// ESP-via-DevKit-autoreset; the only case it's wrong is when the
    /// caller knows the chip is a native ESP USB CDC AND wants the
    /// "(false, false) = run firmware" post-open idle. Pass
    /// `Some(BoardFamily::Esp32NativeUsbCdc)` in that case.
    ///
    /// Daemon callers usually pass `None`; that path now infers native
    /// ESP USB CDC from the OS-reported VID/PID before applying the
    /// `(true, true)` unknown-port fallback.
    pub async fn open_port(
        &self,
        port: &str,
        baud_rate: u32,
        client_id: &str,
        family: Option<crate::boards::BoardFamily>,
        client_metadata: Option<SerialClientMetadata>,
    ) -> fbuild_core::Result<()> {
        let session_key = self.resolve_port_key(port);
        // If already open, just return Ok
        if let Some(mut session) = self.sessions.get_mut(&session_key) {
            if session.is_open {
                session.last_activity_at = now_unix_secs();
                if let Some(metadata) = client_metadata {
                    session
                        .client_metadata
                        .insert(client_id.to_string(), metadata);
                }
                drop(session);
                self.bump_close_generation(&session_key);
                tracing::info!(port, client_id, "port already open, reusing");
                return Ok(());
            }
        }

        // Retry budget: USB re-enumeration on Windows after esptool reset
        // typically completes in <2s; flash uploads finish in <5s. Cap the
        // total wait at ~15s so that permanent failures (port doesn't exist,
        // permission denied, no device) bubble up quickly instead of stalling
        // the daemon's WebSocket clients for 4+ minutes. The previous schedule
        // had 30 retries × ~10s ≈ 5 minutes which deadlocked self-eviction.
        let max_retries: usize = if cfg!(windows) { 8 } else { 6 };
        let backoff_schedule = [250u64, 500, 1000, 2000, 3000]; // ms

        let port_name = port.to_string();
        let mut last_err = String::new();

        for attempt in 0..max_retries {
            let timeout_ms = 100;
            // serialport::open() and DTR/RTS toggling are synchronous Win32 /
            // POSIX system calls. Running them directly inside an `async fn`
            // pins a tokio worker thread for the duration of `CreateFile`
            // (which on Windows can stall multiple seconds during USB
            // re-enumeration). Move the blocking work to a dedicated blocking
            // pool thread so other tokio tasks (WebSocket forwarding,
            // self-eviction tick, HTTP handlers) keep making progress. See
            // ISSUES.md "Issue C".
            let port_for_open = port_name.clone();
            let explicit_family = family;
            let open_result: std::result::Result<
                std::result::Result<Box<dyn serialport::SerialPort>, serialport::Error>,
                tokio::task::JoinError,
            > = tokio::task::spawn_blocking(move || {
                let family_for_open =
                    explicit_family.or_else(|| crate::boards::family_for_port(&port_for_open));
                // FastLED/fbuild#893 acceptance: every attach logs the
                // (port, family, dtr, rts) tuple at info! so a downstream
                // diagnosis (e.g. AutoResearch on ESP32-S3) can verify the
                // attach honored the inferred board-family DTR/RTS rather
                // than the universal (true, true) fallback. Inferring
                // `(false, false)` for ESP native USB CDC vs the default
                // is the difference between firmware running vs stuck in
                // boot-mode after deploy.
                let (preview_dtr, preview_rts) = family_for_open
                    .map(|f| f.idle_dtr_rts())
                    .unwrap_or((true, true));
                tracing::info!(
                    port = %port_for_open,
                    family = ?family_for_open,
                    dtr = preview_dtr,
                    rts = preview_rts,
                    "serial_manager: opening port (family inferred from VID/PID, idle_dtr_rts applied at open)"
                );
                let mut serial = serialport::new(&port_for_open, baud_rate)
                    .timeout(Duration::from_millis(timeout_ms))
                    .open()?;
                // Set the post-open DTR/RTS idle state. Failures here are
                // non-fatal — some adapters (e.g. CP210x in CDC mode) reject
                // the request but the port is still usable. Log both the
                // success and failure paths at `debug!` so a complete log
                // scan can reconstruct the DTR/RTS history end-to-end
                // (FastLED/fbuild#532 acceptance: "logs show enough DTR/RTS
                // /reset context to diagnose future S3 boot-mode lockups").
                //
                // `family.idle_dtr_rts()` (FastLED/fbuild#687) picks
                // `(false, false)` for ESP native USB CDC (post-reset idle
                // = run firmware) vs `(true, true)` for CDC-ACM bridges,
                // Teensy, RP2040, SAMD, Arduino (host-ready / no
                // accidental reset). `None` falls back to `(true, true)`
                // — the universal safe default per the LPC845-BRK
                // incident (FastLED/FastLED#3300). For the full per-chip
                // matrix see `docs/usb-cdc-control-line-matrix.md`
                // (FastLED/fbuild#689).
                let (dtr, rts) = family_for_open
                    .map(|f| f.idle_dtr_rts())
                    .unwrap_or((true, true));
                match serial.write_data_terminal_ready(dtr) {
                    Ok(()) => tracing::debug!(
                        family = ?family_for_open,
                        "manager: open-time DTR={dtr} asserted"
                    ),
                    Err(e) => tracing::warn!("failed to set DTR={dtr}: {}", e),
                }
                match serial.write_request_to_send(rts) {
                    Ok(()) => tracing::debug!(
                        family = ?family_for_open,
                        "manager: open-time RTS={rts} asserted"
                    ),
                    Err(e) => tracing::warn!("failed to set RTS={rts}: {}", e),
                }
                Ok(serial)
            })
            .await;

            let open_inner = match open_result {
                Ok(inner) => inner,
                Err(join_err) => {
                    last_err = format!("open task panicked: {}", join_err);
                    let backoff_idx = attempt.min(backoff_schedule.len() - 1);
                    let backoff_ms = backoff_schedule[backoff_idx];
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
            };

            match open_inner {
                Ok(serial) => {
                    let serial_handle = Arc::new(Mutex::new(serial));
                    let stop_flag = Arc::new(AtomicBool::new(false));

                    let (tx, _rx) = broadcast::channel(BROADCAST_CHANNEL_SIZE);
                    self.broadcasters.insert(port_name.clone(), tx.clone());

                    // Create shared output buffer for the background reader
                    let port_buf = Arc::new(PortOutputBuffer {
                        buffer: std::sync::Mutex::new(VecDeque::with_capacity(OUTPUT_BUFFER_CAP)),
                        total_bytes_read: std::sync::atomic::AtomicU64::new(0),
                        last_read_at_ms: std::sync::atomic::AtomicU64::new(0),
                    });
                    self.output_buffers
                        .insert(port_name.clone(), Arc::clone(&port_buf));

                    // Spawn background reader
                    let reader_handle = {
                        let serial_clone = Arc::clone(&serial_handle);
                        let stop_clone = Arc::clone(&stop_flag);
                        let port_clone = port_name.clone();

                        tokio::task::spawn_blocking(move || {
                            let mut buf = [0u8; READ_BUF_SIZE];
                            let mut partial_line = String::new();

                            while !stop_clone.load(Ordering::Relaxed) {
                                let read_result = {
                                    let mut serial = serial_clone.blocking_lock();
                                    serial.read(&mut buf)
                                };

                                match read_result {
                                    Ok(n) if n > 0 => {
                                        let text = String::from_utf8_lossy(&buf[..n]);
                                        partial_line.push_str(&text);

                                        // Update bytes read
                                        port_buf
                                            .total_bytes_read
                                            .fetch_add(n as u64, Ordering::Relaxed);
                                        port_buf
                                            .last_read_at_ms
                                            .store(now_unix_millis(), Ordering::Relaxed);

                                        // Split into complete lines
                                        while let Some(newline_pos) = partial_line.find('\n') {
                                            let line =
                                                partial_line[..newline_pos].trim_end().to_string();
                                            partial_line =
                                                partial_line[newline_pos + 1..].to_string();

                                            if line.is_empty() {
                                                continue;
                                            }

                                            // Broadcast the line
                                            let _ = tx.send(SerialStreamEvent::Data(line.clone()));

                                            // Append to output buffer
                                            if let Ok(mut ob) = port_buf.buffer.lock() {
                                                if ob.len() >= OUTPUT_BUFFER_CAP {
                                                    ob.pop_front();
                                                }
                                                ob.push_back(line);
                                            }
                                        }
                                    }
                                    Ok(_) => {
                                        // Zero bytes — sleep briefly to avoid busy loop
                                        std::thread::sleep(Duration::from_millis(10));
                                    }
                                    Err(ref e)
                                        if e.kind() == std::io::ErrorKind::TimedOut
                                            || e.kind() == std::io::ErrorKind::WouldBlock =>
                                    {
                                        // Normal timeout, continue
                                        std::thread::sleep(Duration::from_millis(10));
                                    }
                                    Err(e) => {
                                        let message = e.to_string();
                                        tracing::error!(
                                            port = port_clone,
                                            "serial read error: {}",
                                            message
                                        );
                                        let _ = tx.send(SerialStreamEvent::PortDisconnected {
                                            port: port_clone.clone(),
                                            reason: "read_error".to_string(),
                                            message,
                                        });
                                        break;
                                    }
                                }
                            }
                            tracing::info!(port = port_clone, "background reader stopped");
                        })
                    };

                    let mut session = SerialSession::new(port_name.clone(), baud_rate);
                    let now = now_unix_secs();
                    session.is_open = true;
                    session.owner_client_id = Some(client_id.to_string());
                    if let Some(metadata) = client_metadata {
                        session
                            .client_metadata
                            .insert(client_id.to_string(), metadata);
                    }
                    session.serial_handle = Some(serial_handle);
                    session.reader_handle = Some(reader_handle);
                    session.stop_flag = stop_flag;
                    session.started_at = now;
                    session.last_activity_at = now;

                    self.sessions.insert(port_name.clone(), session);
                    self.bump_close_generation(&port_name);

                    tracing::info!(port, client_id, attempt, "port opened successfully");
                    return Ok(());
                }
                Err(e) => {
                    last_err = e.to_string();
                    let backoff_idx = attempt.min(backoff_schedule.len() - 1);
                    let backoff_ms = backoff_schedule[backoff_idx];
                    tracing::debug!(
                        port,
                        attempt,
                        backoff_ms,
                        "open failed: {}, retrying",
                        last_err
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }

        Err(fbuild_core::FbuildError::SerialError(format!(
            "failed to open {} after {} attempts: {}",
            port, max_retries, last_err
        )))
    }

    /// Write data to a serial port. Caller must hold writer lock.
    ///
    /// Both `write()` and `flush()` are synchronous Win32/POSIX syscalls.
    /// To keep the tokio reactor free and to bound how long a wedged
    /// USB-CDC adapter can pin a worker, the blocking call is moved to
    /// `tokio::task::spawn_blocking` (matching the `open_port` and
    /// `esp_hard_reset` pattern) and wrapped in an outer
    /// `tokio::time::timeout` budget. See FastLED/fbuild#803 HIGH
    /// finding.
    pub async fn write_to_port(
        &self,
        port: &str,
        data: &[u8],
        client_id: &str,
    ) -> fbuild_core::Result<usize> {
        let session_key = self.resolve_port_key(port);
        // Verify caller holds writer lock
        let handle = {
            let session = self.sessions.get(&session_key).ok_or_else(|| {
                fbuild_core::FbuildError::SerialError(format!("port {} not open", port))
            })?;
            if session.writer_client_id.as_deref() != Some(client_id) {
                return Err(fbuild_core::FbuildError::SerialError(format!(
                    "client {} does not hold writer lock on {}",
                    client_id, port
                )));
            }
            session
                .serial_handle
                .as_ref()
                .ok_or_else(|| {
                    fbuild_core::FbuildError::SerialError(format!(
                        "port {} has no serial handle",
                        port
                    ))
                })?
                .clone()
        };

        let port_owned = port.to_string();
        let data_owned = data.to_vec();
        let expected_len = data_owned.len();
        let write_future = tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut serial = handle.blocking_lock();
            serial
                .write_all(&data_owned)
                .map_err(|e| format!("write failed: {}", e))?;
            serial.flush().map_err(|e| format!("flush failed: {}", e))?;
            Ok::<usize, String>(expected_len)
        });

        // 2s budget: covers normal Windows USB-CDC drain + headroom but
        // bounds the worst case so a wedged adapter cannot stall the
        // daemon indefinitely on a write/flush syscall.
        let join_result = tokio::time::timeout(Duration::from_secs(2), write_future)
            .await
            .map_err(|_| {
                fbuild_core::FbuildError::SerialError(format!(
                    "write to {} timed out after 2s",
                    port_owned
                ))
            })?;

        let bytes_written = match join_result {
            Ok(Ok(n)) => n,
            Ok(Err(msg)) => return Err(fbuild_core::FbuildError::SerialError(msg)),
            Err(join_err) => {
                return Err(fbuild_core::FbuildError::SerialError(format!(
                    "write task panicked on {}: {}",
                    port_owned, join_err
                )));
            }
        };

        // Update stats
        if let Some(mut session) = self.sessions.get_mut(&session_key) {
            let now = now_unix_secs();
            session.total_bytes_written += bytes_written as u64;
            session.last_write_at = Some(now);
            session.last_activity_at = now;
        }

        Ok(bytes_written)
    }

    /// Pulse the ESP DTR/RTS hard-reset sequence on `port` to bring an
    /// ESP chip out of ROM download mode and back into normal firmware
    /// boot — the recovery counterpart of
    /// [`crate::boot_mode::detect_download_mode`].
    ///
    /// The caller must **own** the port (i.e. opened it via
    /// [`SharedSerialManager::open_port`] with this `client_id`). Unlike
    /// [`SharedSerialManager::write_to_port`] this does NOT require a
    /// writer lock — by the time auto-recovery is invoked, detection has
    /// already established that the board is stuck in ROM, and a competing
    /// writer would have nothing useful to do anyway.
    ///
    /// Wraps [`crate::esp_reset::esp_hard_reset_blocking`] in `spawn_blocking`
    /// because every `serialport` line-control call is a synchronous
    /// Win32/POSIX syscall — matches the pattern in
    /// [`SharedSerialManager::open_port`].
    ///
    /// See FastLED/fbuild#532 (auto-recover-from-download-mode path).
    pub async fn esp_hard_reset(&self, port: &str, client_id: &str) -> fbuild_core::Result<()> {
        let session_key = self.resolve_port_key(port);
        let handle = {
            let session = self.sessions.get(&session_key).ok_or_else(|| {
                fbuild_core::FbuildError::SerialError(format!("port {} not open", port))
            })?;
            if session.owner_client_id.as_deref() != Some(client_id) {
                return Err(fbuild_core::FbuildError::SerialError(format!(
                    "client {} does not own {} (cannot issue ESP hard-reset)",
                    client_id, port
                )));
            }
            session
                .serial_handle
                .as_ref()
                .ok_or_else(|| {
                    fbuild_core::FbuildError::SerialError(format!(
                        "port {} has no serial handle",
                        port
                    ))
                })?
                .clone()
        };

        let port_owned = port.to_string();
        let port_for_log = port_owned.clone();
        let reset_future = tokio::task::spawn_blocking(move || {
            tracing::info!(
                port = port_for_log,
                "esp_hard_reset: starting DTR/RTS recovery sequence"
            );
            let mut guard = handle.blocking_lock();
            crate::esp_reset::esp_hard_reset_blocking(&mut **guard)
        });

        // 3s budget: HARD_RESET_PULSE_MS is 100ms, plus a few DTR/RTS
        // line-control syscalls (USB control transfers on CDC-ACM). A
        // healthy adapter completes in well under 250ms; the 3s cap
        // bounds the worst case where a misbehaving/unplugged adapter
        // makes a kernel ioctl block (FastLED/fbuild#803 MEDIUM).
        let join_result = tokio::time::timeout(Duration::from_secs(3), reset_future)
            .await
            .map_err(|_| {
                fbuild_core::FbuildError::SerialError(format!(
                    "esp_hard_reset on {} timed out after 3s",
                    port_owned
                ))
            })?;

        match join_result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(fbuild_core::FbuildError::SerialError(format!(
                "esp_hard_reset on {}: {}",
                port_owned, e
            ))),
            Err(join_err) => Err(fbuild_core::FbuildError::SerialError(format!(
                "esp_hard_reset task panicked on {}: {}",
                port_owned, join_err
            ))),
        }
    }

    /// Close a serial port.
    pub async fn close_port(&self, port: &str, client_id: &str) -> fbuild_core::Result<()> {
        let session_key = self.resolve_port_key(port);
        self.bump_close_generation(&session_key);
        if let Some((_, mut session)) = self.sessions.remove(&session_key) {
            // Signal the background reader to stop
            session.stop_flag.store(true, Ordering::Relaxed);

            // Wait for the reader task to finish, but cap how long
            // close can hang. The reader's per-iteration `serial.read()`
            // has a 100ms timeout, so a healthy reader exits well under
            // 1s once `stop_flag` is set. A wedged Windows USB-CDC
            // driver can occasionally ignore the configured read
            // timeout — in that case we leak the JoinHandle and proceed
            // so the daemon stays responsive (FastLED/fbuild#803 HIGH).
            if let Some(handle) = session.reader_handle.take() {
                match tokio::time::timeout(Duration::from_secs(2), handle).await {
                    Ok(_join_result) => {}
                    Err(_) => {
                        tracing::warn!(
                            port,
                            client_id,
                            "reader task did not exit within 2s of close — \
                             leaking JoinHandle and proceeding"
                        );
                    }
                }
            }

            // Drop the serial handle (closes the port)
            session.serial_handle = None;
            session.is_open = false;
        }
        self.broadcasters.remove(&session_key);
        self.output_buffers.remove(&session_key);
        self.close_generations.remove(&session_key);
        self.remove_aliases_for_session(&session_key);
        tracing::info!(port, client_id, "port closed");
        Ok(())
    }

    async fn close_port_if_generation(
        &self,
        port: &str,
        client_id: &str,
        expected_generation: Option<u64>,
    ) -> fbuild_core::Result<bool> {
        let session_key = self.resolve_port_key(port);
        let current_generation = self.close_generation(&session_key);
        if current_generation != expected_generation {
            tracing::info!(
                port = session_key,
                client_id,
                expected_generation = ?expected_generation,
                current_generation = ?current_generation,
                "stale serial port close skipped"
            );
            return Ok(false);
        }
        self.close_port(port, client_id).await?;
        Ok(true)
    }

    /// Schedule physical close after a grace window if the port is still idle.
    ///
    /// This keeps `SerialProxy.close()` logical from the client's point of
    /// view: the subscriber detaches immediately, but a rapid reconnect can
    /// reuse the existing OS handle instead of forcing a USB CDC close/open
    /// cycle. Immediate force-close paths such as deploy preemption use a
    /// generation guard so stale closes cannot tear down a newer session.
    pub fn close_port_after_grace_if_idle(
        self: &Arc<Self>,
        port: &str,
        client_id: &str,
        grace: Duration,
    ) -> bool {
        let session_key = self.resolve_port_key(port);
        if self.has_clients(&session_key) || !self.sessions.contains_key(&session_key) {
            return false;
        }

        let generation = self.bump_close_generation(&session_key);
        let manager = Arc::clone(self);
        let port = session_key;
        let client_id = client_id.to_string();
        tokio::spawn(async move {
            tracing::debug!(
                port,
                client_id,
                grace_ms = grace.as_millis(),
                "scheduled idle serial port close"
            );
            tokio::time::sleep(grace).await;
            let current_generation = manager.close_generation(&port);
            if current_generation != Some(generation) || manager.has_clients(&port) {
                tracing::debug!(
                    port,
                    client_id,
                    "idle serial port close canceled by new activity"
                );
                return;
            }
            if let Err(err) = manager.close_port(&port, &client_id).await {
                tracing::warn!(port, client_id, "delayed close failed: {}", err);
            }
        });
        true
    }

    /// Attach a reader to receive broadcast output.
    ///
    /// All-or-nothing: returns `None` without mutating session state if
    /// the port has no active broadcaster, so callers that fail to attach
    /// don't leave a dangling `reader_client_ids` entry that would block
    /// self-eviction. See FastLED/fbuild#51.
    pub fn attach_reader(
        &self,
        port: &str,
        client_id: &str,
        client_metadata: Option<SerialClientMetadata>,
    ) -> Option<broadcast::Receiver<SerialStreamEvent>> {
        let session_key = self.resolve_port_key(port);
        let rx = self
            .broadcasters
            .get(&session_key)
            .map(|tx| tx.subscribe())?;
        if let Some(mut session) = self.sessions.get_mut(&session_key) {
            session.last_activity_at = now_unix_secs();
            session.reader_client_ids.insert(client_id.to_string());
            if let Some(metadata) = client_metadata {
                session
                    .client_metadata
                    .insert(client_id.to_string(), metadata);
            }
            drop(session);
            self.bump_close_generation(&session_key);
        }
        Some(rx)
    }

    /// Detach a reader.
    pub fn detach_reader(&self, port: &str, client_id: &str) {
        let session_key = self.resolve_port_key(port);
        if let Some(mut session) = self.sessions.get_mut(&session_key) {
            session.reader_client_ids.remove(client_id);
            session.last_activity_at = now_unix_secs();
        }
    }

    /// Notify subscribers on the old port that a tracked USB serial moved to a
    /// new OS port and is available again there.
    pub fn notify_port_renumbered(
        &self,
        old_port: &str,
        new_port: &str,
        reason: &str,
        serial: Option<String>,
    ) -> bool {
        let session_key = self.resolve_port_key(old_port);
        let Some(tx) = self.broadcasters.get(&session_key) else {
            return false;
        };
        let sent_renumbered = tx
            .send(SerialStreamEvent::PortRenumbered {
                port: session_key,
                new_port: new_port.to_string(),
                reason: reason.to_string(),
                serial,
            })
            .is_ok();
        let sent_reattached = tx
            .send(SerialStreamEvent::PortReattached {
                port: new_port.to_string(),
                previous_port: old_port.to_string(),
            })
            .is_ok();
        sent_renumbered || sent_reattached
    }

    pub fn notify_port_rebind_failed(
        &self,
        old_port: &str,
        new_port: &str,
        reason: &str,
        message: String,
    ) -> bool {
        let session_key = self.resolve_port_key(old_port);
        let Some(tx) = self.broadcasters.get(&session_key) else {
            return false;
        };
        tx.send(SerialStreamEvent::PortRebindFailed {
            port: session_key,
            new_port: new_port.to_string(),
            reason: reason.to_string(),
            message,
        })
        .is_ok()
    }

    /// Reopen the physical serial handle on `new_port` while preserving the
    /// existing logical session keyed by `old_port`.
    pub async fn rebind_port_session(
        &self,
        old_port: &str,
        new_port: &str,
        reason: &str,
        serial: Option<String>,
    ) -> fbuild_core::Result<bool> {
        let session_key = self.resolve_port_key(old_port);
        if !self.sessions.contains_key(&session_key)
            || !self.broadcasters.contains_key(&session_key)
        {
            return Ok(false);
        }

        let baud_rate = self
            .sessions
            .get(&session_key)
            .map(|session| session.baud_rate)
            .ok_or_else(|| {
                fbuild_core::FbuildError::SerialError(format!(
                    "serial session {} disappeared during rebind",
                    session_key
                ))
            })?;
        let max_retries: usize = if cfg!(windows) { 8 } else { 6 };
        let serial_handle = Arc::new(Mutex::new(
            Self::open_physical_serial(new_port, baud_rate, max_retries).await?,
        ));

        self.rebind_port_session_to_handle(&session_key, new_port, serial_handle, reason, serial)
            .await
    }

    async fn rebind_port_session_to_handle(
        &self,
        session_key: &str,
        new_port: &str,
        serial_handle: Arc<Mutex<Box<dyn serialport::SerialPort>>>,
        reason: &str,
        serial: Option<String>,
    ) -> fbuild_core::Result<bool> {
        let Some(tx) = self.broadcasters.get(session_key).map(|tx| tx.clone()) else {
            return Ok(false);
        };
        let port_buf = self
            .output_buffers
            .entry(session_key.to_string())
            .or_insert_with(|| {
                Arc::new(PortOutputBuffer {
                    buffer: std::sync::Mutex::new(VecDeque::with_capacity(OUTPUT_BUFFER_CAP)),
                    total_bytes_read: std::sync::atomic::AtomicU64::new(0),
                    last_read_at_ms: std::sync::atomic::AtomicU64::new(0),
                })
            })
            .clone();

        let old_reader = if let Some(mut session) = self.sessions.get_mut(session_key) {
            session.stop_flag.store(true, Ordering::Relaxed);
            session.reader_handle.take()
        } else {
            return Ok(false);
        };
        if let Some(handle) = old_reader {
            // Bound how long the rebind can stall on a wedged reader.
            // See `close_port` for the same rationale
            // (FastLED/fbuild#803 HIGH).
            match tokio::time::timeout(Duration::from_secs(2), handle).await {
                Ok(_) => {}
                Err(_) => {
                    tracing::warn!(
                        port = session_key,
                        new_port,
                        "old reader did not exit within 2s during rebind — \
                         leaking JoinHandle and proceeding"
                    );
                }
            }
        }

        let stop_flag = Arc::new(AtomicBool::new(false));
        let reader_handle = Self::spawn_reader(
            session_key.to_string(),
            Arc::clone(&serial_handle),
            Arc::clone(&stop_flag),
            tx.clone(),
            port_buf,
        );

        if let Some(mut session) = self.sessions.get_mut(session_key) {
            session.port = new_port.to_string();
            session.is_open = true;
            session.serial_handle = Some(serial_handle);
            session.reader_handle = Some(reader_handle);
            session.stop_flag = stop_flag;
        }
        self.port_aliases
            .insert(new_port.to_string(), session_key.to_string());
        self.bump_close_generation(session_key);
        let _ = tx.send(SerialStreamEvent::PortRenumbered {
            port: session_key.to_string(),
            new_port: new_port.to_string(),
            reason: reason.to_string(),
            serial,
        });
        let _ = tx.send(SerialStreamEvent::PortReattached {
            port: new_port.to_string(),
            previous_port: session_key.to_string(),
        });
        Ok(true)
    }

    /// Returns the number of attached readers for a port (0 if not open).
    pub fn reader_count(&self, port: &str) -> usize {
        let session_key = self.resolve_port_key(port);
        self.sessions
            .get(&session_key)
            .map(|s| s.reader_client_ids.len())
            .unwrap_or(0)
    }

    /// Returns true if a port has any active reader or writer client.
    pub fn has_clients(&self, port: &str) -> bool {
        let session_key = self.resolve_port_key(port);
        self.sessions
            .get(&session_key)
            .map(|s| !s.reader_client_ids.is_empty() || s.writer_client_id.is_some())
            .unwrap_or(false)
    }

    /// Acquire exclusive write access to a port.
    pub async fn acquire_writer(&self, port: &str, client_id: &str) -> fbuild_core::Result<()> {
        let session_key = self.resolve_port_key(port);
        if let Some(mut session) = self.sessions.get_mut(&session_key) {
            if session.writer_client_id.is_some() {
                return Err(fbuild_core::FbuildError::SerialError(format!(
                    "port {} already has an exclusive writer",
                    port
                )));
            }
            session.writer_client_id = Some(client_id.to_string());
            session.last_activity_at = now_unix_secs();
            drop(session);
            self.bump_close_generation(&session_key);
            Ok(())
        } else {
            Err(fbuild_core::FbuildError::SerialError(format!(
                "port {} not open",
                port
            )))
        }
    }

    /// Release write access.
    pub fn release_writer(&self, port: &str, client_id: &str) {
        let session_key = self.resolve_port_key(port);
        if let Some(mut session) = self.sessions.get_mut(&session_key) {
            if session.writer_client_id.as_deref() == Some(client_id) {
                session.writer_client_id = None;
                session.last_activity_at = now_unix_secs();
            }
        }
    }

    /// Force-close for deploy preemption.
    pub async fn preempt_for_deploy(
        &self,
        port: &str,
        reason: String,
        preempted_by: String,
    ) -> fbuild_core::Result<()> {
        let session_key = self.resolve_port_key(port);
        let generation = self.close_generation(&session_key);
        self.preemption
            .preempt(&session_key, reason, preempted_by)
            .await;
        self.close_port_if_generation(&session_key, "deploy_preemption", generation)
            .await?;
        Ok(())
    }

    /// Clear preemption after deploy completes.
    pub async fn clear_preemption(&self, port: &str) {
        let session_key = self.resolve_port_key(port);
        self.preemption.clear(&session_key).await;
    }

    /// Check if a port is preempted.
    pub async fn is_preempted(&self, port: &str) -> bool {
        let session_key = self.resolve_port_key(port);
        self.preemption.is_preempted(&session_key).await
    }

    /// Get the preemption tracker for external use.
    pub fn preemption_tracker(&self) -> &Arc<PreemptionTracker> {
        &self.preemption
    }

    /// Attach a crash decoder to a port for decoding crash stack traces.
    pub fn set_crash_decoder(&self, port: &str, decoder: CrashDecoder) {
        let session_key = self.resolve_port_key(port);
        self.crash_decoders.insert(session_key, decoder);
    }

    /// Remove crash decoder from a port.
    pub fn remove_crash_decoder(&self, port: &str) {
        let session_key = self.resolve_port_key(port);
        self.crash_decoders.remove(&session_key);
    }

    /// Process a serial line through the crash decoder for a port.
    ///
    /// Returns decoded crash trace lines if a crash dump just completed.
    ///
    /// The decoder is temporarily removed from the DashMap so the shard
    /// lock isn't held across the `addr2line` `.await`. Re-inserted on
    /// completion; the brief race window is acceptable because the
    /// caller (the serial-monitor reader task) is the only producer for
    /// a given port.
    pub async fn process_crash_line(&self, port: &str, line: &str) -> Option<Vec<String>> {
        let session_key = self.resolve_port_key(port);
        let (key, mut decoder) = self.crash_decoders.remove(&session_key)?;
        let result = decoder.process_line(line).await;
        self.crash_decoders.insert(key, decoder);
        result
    }

    /// Get a snapshot of all active serial port sessions for lock/status reporting.
    pub fn get_port_sessions(&self) -> Vec<PortSessionInfo> {
        self.sessions
            .iter()
            .map(|entry| {
                let s = entry.value();
                let last_read_at = self.output_buffers.get(entry.key()).and_then(|buf| {
                    millis_to_unix_secs(buf.last_read_at_ms.load(Ordering::Relaxed))
                });
                let total_bytes_read = self
                    .output_buffers
                    .get(entry.key())
                    .map(|buf| buf.total_bytes_read.load(Ordering::Relaxed))
                    .unwrap_or(s.total_bytes_read);
                let mut reader_client_ids: Vec<String> =
                    s.reader_client_ids.iter().cloned().collect();
                reader_client_ids.sort();
                let mut clients: Vec<SerialClientInfo> = s
                    .client_metadata
                    .iter()
                    .map(|(client_id, metadata)| SerialClientInfo {
                        client_id: client_id.clone(),
                        metadata: metadata.clone(),
                    })
                    .collect();
                clients.sort_by(|a, b| a.client_id.cmp(&b.client_id));
                PortSessionInfo {
                    port: s.port.clone(),
                    is_open: s.is_open,
                    writer_client_id: s.writer_client_id.clone(),
                    reader_count: s.reader_client_ids.len(),
                    reader_client_ids,
                    owner_client_id: s.owner_client_id.clone(),
                    baud_rate: s.baud_rate,
                    started_at: s.started_at,
                    last_activity_at: s.last_activity_at,
                    last_read_at,
                    last_write_at: s.last_write_at,
                    total_bytes_read,
                    total_bytes_written: s.total_bytes_written,
                    clients,
                }
            })
            .collect()
    }

    /// Test hook for higher-level daemon handlers that need an in-memory
    /// serial session without opening a real OS port.
    #[cfg(any(test, debug_assertions))]
    pub fn insert_session_for_test(&self, session: SerialSession) {
        self.sessions.insert(session.port.clone(), session);
    }

    async fn open_physical_serial(
        port: &str,
        baud_rate: u32,
        max_retries: usize,
    ) -> fbuild_core::Result<Box<dyn serialport::SerialPort>> {
        let backoff_schedule = [250u64, 500, 1000, 2000, 3000];
        let mut last_err = String::new();

        for attempt in 0..max_retries {
            let port_for_open = port.to_string();
            let open_result: std::result::Result<
                std::result::Result<Box<dyn serialport::SerialPort>, serialport::Error>,
                tokio::task::JoinError,
            > = tokio::task::spawn_blocking(move || {
                let family_for_open = crate::boards::family_for_port(&port_for_open);
                let (dtr, rts) = family_for_open
                    .map(|family| family.idle_dtr_rts())
                    .unwrap_or((true, true));
                let mut serial = serialport::new(&port_for_open, baud_rate)
                    .timeout(Duration::from_millis(100))
                    .open()?;
                match serial.write_data_terminal_ready(dtr) {
                    Ok(()) => tracing::debug!(
                        family = ?family_for_open,
                        "manager: open-time DTR={dtr} asserted"
                    ),
                    Err(e) => tracing::warn!("failed to set DTR: {}", e),
                }
                match serial.write_request_to_send(rts) {
                    Ok(()) => tracing::debug!(
                        family = ?family_for_open,
                        "manager: open-time RTS={rts} asserted"
                    ),
                    Err(e) => tracing::warn!("failed to set RTS: {}", e),
                }
                Ok(serial)
            })
            .await;

            match open_result {
                Ok(Ok(serial)) => return Ok(serial),
                Ok(Err(e)) => last_err = e.to_string(),
                Err(join_err) => last_err = format!("open task panicked: {}", join_err),
            }

            let backoff_idx = attempt.min(backoff_schedule.len() - 1);
            let backoff_ms = backoff_schedule[backoff_idx];
            tracing::debug!(
                port,
                attempt,
                backoff_ms,
                "open failed: {}, retrying",
                last_err
            );
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        }

        Err(fbuild_core::FbuildError::SerialError(format!(
            "failed to open {} after {} attempts: {}",
            port, max_retries, last_err
        )))
    }

    fn spawn_reader(
        event_port: String,
        serial_handle: Arc<Mutex<Box<dyn serialport::SerialPort>>>,
        stop_flag: Arc<AtomicBool>,
        tx: broadcast::Sender<SerialStreamEvent>,
        port_buf: Arc<PortOutputBuffer>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; READ_BUF_SIZE];
            let mut partial_line = String::new();

            while !stop_flag.load(Ordering::Relaxed) {
                let read_result = {
                    let mut serial = serial_handle.blocking_lock();
                    serial.read(&mut buf)
                };

                match read_result {
                    Ok(n) if n > 0 => {
                        let text = String::from_utf8_lossy(&buf[..n]);
                        partial_line.push_str(&text);
                        port_buf
                            .total_bytes_read
                            .fetch_add(n as u64, Ordering::Relaxed);
                        port_buf
                            .last_read_at_ms
                            .store(now_unix_millis(), Ordering::Relaxed);

                        while let Some(newline_pos) = partial_line.find('\n') {
                            let line = partial_line[..newline_pos].trim_end().to_string();
                            partial_line = partial_line[newline_pos + 1..].to_string();

                            if line.is_empty() {
                                continue;
                            }

                            let _ = tx.send(SerialStreamEvent::Data(line.clone()));
                            if let Ok(mut ob) = port_buf.buffer.lock() {
                                if ob.len() >= OUTPUT_BUFFER_CAP {
                                    ob.pop_front();
                                }
                                ob.push_back(line);
                            }
                        }
                    }
                    Ok(_) => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(ref e)
                        if e.kind() == std::io::ErrorKind::TimedOut
                            || e.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => {
                        let message = e.to_string();
                        tracing::error!(port = event_port, "serial read error: {}", message);
                        let _ = tx.send(SerialStreamEvent::PortDisconnected {
                            port: event_port.clone(),
                            reason: "read_error".to_string(),
                            message,
                        });
                        break;
                    }
                }
            }
            tracing::info!(port = event_port, "background reader stopped");
        })
    }

    fn resolve_port_key(&self, port: &str) -> String {
        self.port_aliases
            .get(port)
            .map(|alias| alias.clone())
            .unwrap_or_else(|| port.to_string())
    }

    fn remove_aliases_for_session(&self, session_key: &str) {
        let aliases: Vec<String> = self
            .port_aliases
            .iter()
            .filter_map(|entry| (entry.value() == session_key).then(|| entry.key().clone()))
            .collect();
        for alias in aliases {
            self.port_aliases.remove(&alias);
        }
    }

    fn bump_close_generation(&self, port: &str) -> u64 {
        let mut generation = self.close_generations.entry(port.to_string()).or_insert(0);
        *generation += 1;
        *generation
    }

    fn close_generation(&self, port: &str) -> Option<u64> {
        self.close_generations
            .get(port)
            .map(|generation| *generation)
    }
}

/// Snapshot of a serial port session for status reporting.
#[derive(Debug, Clone)]
pub struct PortSessionInfo {
    pub port: String,
    pub is_open: bool,
    pub writer_client_id: Option<String>,
    pub reader_count: usize,
    pub reader_client_ids: Vec<String>,
    pub owner_client_id: Option<String>,
    pub baud_rate: u32,
    pub started_at: f64,
    pub last_activity_at: f64,
    pub last_read_at: Option<f64>,
    pub last_write_at: Option<f64>,
    pub total_bytes_read: u64,
    pub total_bytes_written: u64,
    pub clients: Vec<SerialClientInfo>,
}

/// Snapshot of a serial client attached to a port session.
#[derive(Debug, Clone)]
pub struct SerialClientInfo {
    pub client_id: String,
    pub metadata: SerialClientMetadata,
}

impl Default for SharedSerialManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;

//! SharedSerialManager: centralized serial port access.
//!
//! This is the Rust equivalent of Python's SharedSerialManager (1170 lines).
//! All serial I/O flows through this single manager in the daemon.
//!
//! ## Concurrency Model
//!
//! - Per-port state protected by tokio::sync::Mutex
//! - Background reader task per open port (tokio::spawn)
//! - Broadcast channel for output distribution to readers
//! - Exclusive writer access via condition variable pattern
//!
//! ## Windows USB-CDC Strategy (v5)
//!
//! 1. Drain input buffer aggressively (1 second initial)
//! 2. Per-attempt: drain input buffer before each write
//! 3. 50ms per-attempt timeout (many rapid attempts)
//! 4. 200 max attempts in 20 seconds
//! 5. Toggle DTR/RTS for flow control

use crate::crash_decoder::CrashDecoder;
use crate::messages::SerialStreamEvent;
use crate::preemption::PreemptionTracker;
use crate::session::SerialSession;
use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};

const OUTPUT_BUFFER_CAP: usize = 10_000;
const BROADCAST_CHANNEL_SIZE: usize = 1024;
const READ_BUF_SIZE: usize = 4096;

/// Per-port output buffer shared with the background reader. Separate from
/// `SerialSession` so we don't need `SerialSession: Clone`.
struct PortOutputBuffer {
    buffer: std::sync::Mutex<VecDeque<String>>,
    total_bytes_read: std::sync::atomic::AtomicU64,
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
    pub async fn open_port(
        &self,
        port: &str,
        baud_rate: u32,
        client_id: &str,
        family: Option<crate::boards::BoardFamily>,
    ) -> fbuild_core::Result<()> {
        let session_key = self.resolve_port_key(port);
        // If already open, just return Ok
        if let Some(session) = self.sessions.get(&session_key) {
            if session.is_open {
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
            let family_for_open = family;
            let open_result: std::result::Result<
                std::result::Result<Box<dyn serialport::SerialPort>, serialport::Error>,
                tokio::task::JoinError,
            > = tokio::task::spawn_blocking(move || {
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
                let (dtr, rts) = family.map(|f| f.idle_dtr_rts()).unwrap_or((true, true));
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
                    session.is_open = true;
                    session.owner_client_id = Some(client_id.to_string());
                    session.serial_handle = Some(serial_handle);
                    session.reader_handle = Some(reader_handle);
                    session.stop_flag = stop_flag;
                    session.started_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();

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

        let mut serial = handle.lock().await;
        use std::io::Write;
        let bytes_written = serial
            .write(data)
            .map_err(|e| fbuild_core::FbuildError::SerialError(format!("write failed: {}", e)))?;
        serial
            .flush()
            .map_err(|e| fbuild_core::FbuildError::SerialError(format!("flush failed: {}", e)))?;
        drop(serial);

        // Update stats
        if let Some(mut session) = self.sessions.get_mut(&session_key) {
            session.total_bytes_written += bytes_written as u64;
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
        let join_result = tokio::task::spawn_blocking(move || {
            tracing::info!(
                port = port_for_log,
                "esp_hard_reset: starting DTR/RTS recovery sequence"
            );
            let mut guard = handle.blocking_lock();
            crate::esp_reset::esp_hard_reset_blocking(&mut **guard)
        })
        .await;

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

            // Wait for the reader task to finish
            if let Some(handle) = session.reader_handle.take() {
                let _ = handle.await;
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

    /// Schedule physical close after a grace window if the port is still idle.
    ///
    /// This keeps `SerialProxy.close()` logical from the client's point of
    /// view: the subscriber detaches immediately, but a rapid reconnect can
    /// reuse the existing OS handle instead of forcing a USB CDC close/open
    /// cycle. Immediate force-close paths such as deploy preemption still call
    /// [`Self::close_port`] directly.
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
    ) -> Option<broadcast::Receiver<SerialStreamEvent>> {
        let session_key = self.resolve_port_key(port);
        let rx = self
            .broadcasters
            .get(&session_key)
            .map(|tx| tx.subscribe())?;
        if let Some(mut session) = self.sessions.get_mut(&session_key) {
            session.reader_client_ids.insert(client_id.to_string());
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
            let _ = handle.await;
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
        self.preemption
            .preempt(&session_key, reason, preempted_by)
            .await;
        self.close_port(port, "deploy_preemption").await?;
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
    pub fn process_crash_line(&self, port: &str, line: &str) -> Option<Vec<String>> {
        let session_key = self.resolve_port_key(port);
        self.crash_decoders
            .get_mut(&session_key)
            .and_then(|mut decoder| decoder.process_line(line))
    }

    /// Get a snapshot of all active serial port sessions for lock/status reporting.
    pub fn get_port_sessions(&self) -> Vec<PortSessionInfo> {
        self.sessions
            .iter()
            .map(|entry| {
                let s = entry.value();
                PortSessionInfo {
                    port: s.port.clone(),
                    is_open: s.is_open,
                    writer_client_id: s.writer_client_id.clone(),
                    reader_count: s.reader_client_ids.len(),
                    owner_client_id: s.owner_client_id.clone(),
                    baud_rate: s.baud_rate,
                }
            })
            .collect()
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
                let mut serial = serialport::new(&port_for_open, baud_rate)
                    .timeout(Duration::from_millis(100))
                    .open()?;
                match serial.write_data_terminal_ready(true) {
                    Ok(()) => tracing::debug!("manager: open-time DTR=high asserted"),
                    Err(e) => tracing::warn!("failed to set DTR: {}", e),
                }
                match serial.write_request_to_send(true) {
                    Ok(()) => tracing::debug!("manager: open-time RTS=high asserted"),
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
    pub owner_client_id: Option<String>,
    pub baud_rate: u32,
}

impl Default for SharedSerialManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
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
}

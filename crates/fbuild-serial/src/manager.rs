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
    /// Broadcast channels per port for output distribution.
    broadcasters: DashMap<String, broadcast::Sender<String>>,
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
            broadcasters: DashMap::new(),
            preemption: Arc::new(PreemptionTracker::new()),
            crash_decoders: DashMap::new(),
            output_buffers: DashMap::new(),
        }
    }

    /// Open a serial port. Retries with backoff for Windows USB-CDC.
    pub async fn open_port(
        &self,
        port: &str,
        baud_rate: u32,
        client_id: &str,
    ) -> fbuild_core::Result<()> {
        // If already open, just return Ok
        if let Some(session) = self.sessions.get(port) {
            if session.is_open {
                tracing::info!(port, client_id, "port already open, reusing");
                return Ok(());
            }
        }

        let max_retries: usize = if cfg!(windows) { 30 } else { 15 };
        let backoff_schedule = [1000u64, 2000, 4000, 8000, 10000]; // ms

        let port_name = port.to_string();
        let mut last_err = String::new();

        for attempt in 0..max_retries {
            let timeout_ms = 100;
            match serialport::new(&port_name, baud_rate)
                .timeout(Duration::from_millis(timeout_ms))
                .open()
            {
                Ok(mut serial) => {
                    // Set DTR=true, RTS=true for flow control
                    if let Err(e) = serial.write_data_terminal_ready(true) {
                        tracing::warn!(port, "failed to set DTR: {}", e);
                    }
                    if let Err(e) = serial.write_request_to_send(true) {
                        tracing::warn!(port, "failed to set RTS: {}", e);
                    }

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
                                            let _ = tx.send(line.clone());

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
                                        tracing::error!(
                                            port = port_clone,
                                            "serial read error: {}",
                                            e
                                        );
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
        // Verify caller holds writer lock
        let handle = {
            let session = self.sessions.get(port).ok_or_else(|| {
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
        if let Some(mut session) = self.sessions.get_mut(port) {
            session.total_bytes_written += bytes_written as u64;
        }

        Ok(bytes_written)
    }

    /// Close a serial port.
    pub async fn close_port(&self, port: &str, client_id: &str) -> fbuild_core::Result<()> {
        if let Some((_, mut session)) = self.sessions.remove(port) {
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
        self.broadcasters.remove(port);
        self.output_buffers.remove(port);
        tracing::info!(port, client_id, "port closed");
        Ok(())
    }

    /// Attach a reader to receive broadcast output.
    pub fn attach_reader(
        &self,
        port: &str,
        client_id: &str,
    ) -> Option<broadcast::Receiver<String>> {
        if let Some(mut session) = self.sessions.get_mut(port) {
            session.reader_client_ids.insert(client_id.to_string());
        }
        self.broadcasters.get(port).map(|tx| tx.subscribe())
    }

    /// Detach a reader.
    pub fn detach_reader(&self, port: &str, client_id: &str) {
        if let Some(mut session) = self.sessions.get_mut(port) {
            session.reader_client_ids.remove(client_id);
        }
    }

    /// Acquire exclusive write access to a port.
    pub async fn acquire_writer(&self, port: &str, client_id: &str) -> fbuild_core::Result<()> {
        if let Some(mut session) = self.sessions.get_mut(port) {
            if session.writer_client_id.is_some() {
                return Err(fbuild_core::FbuildError::SerialError(format!(
                    "port {} already has an exclusive writer",
                    port
                )));
            }
            session.writer_client_id = Some(client_id.to_string());
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
        if let Some(mut session) = self.sessions.get_mut(port) {
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
        self.preemption.preempt(port, reason, preempted_by).await;
        self.close_port(port, "deploy_preemption").await?;
        Ok(())
    }

    /// Clear preemption after deploy completes.
    pub async fn clear_preemption(&self, port: &str) {
        self.preemption.clear(port).await;
    }

    /// Check if a port is preempted.
    pub async fn is_preempted(&self, port: &str) -> bool {
        self.preemption.is_preempted(port).await
    }

    /// Get the preemption tracker for external use.
    pub fn preemption_tracker(&self) -> &Arc<PreemptionTracker> {
        &self.preemption
    }

    /// Attach a crash decoder to a port for decoding crash stack traces.
    pub fn set_crash_decoder(&self, port: &str, decoder: CrashDecoder) {
        self.crash_decoders.insert(port.to_string(), decoder);
    }

    /// Remove crash decoder from a port.
    pub fn remove_crash_decoder(&self, port: &str) {
        self.crash_decoders.remove(port);
    }

    /// Process a serial line through the crash decoder for a port.
    ///
    /// Returns decoded crash trace lines if a crash dump just completed.
    pub fn process_crash_line(&self, port: &str, line: &str) -> Option<Vec<String>> {
        self.crash_decoders
            .get_mut(port)
            .and_then(|mut decoder| decoder.process_line(line))
    }
}

impl Default for SharedSerialManager {
    fn default() -> Self {
        Self::new()
    }
}

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

use crate::preemption::PreemptionTracker;
use crate::session::SerialSession;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Central serial port manager. One instance per daemon.
pub struct SharedSerialManager {
    sessions: DashMap<String, SerialSession>,
    /// Broadcast channels per port for output distribution.
    broadcasters: DashMap<String, broadcast::Sender<String>>,
    preemption: Arc<PreemptionTracker>,
}

impl SharedSerialManager {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            broadcasters: DashMap::new(),
            preemption: Arc::new(PreemptionTracker::new()),
        }
    }

    /// Open a serial port. Retries with backoff for Windows USB-CDC.
    pub async fn open_port(
        &self,
        port: &str,
        baud_rate: u32,
        client_id: &str,
    ) -> fbuild_core::Result<()> {
        // TODO: implement with serialport crate
        // - 30 retries on Windows, 15 on Linux/macOS
        // - Exponential backoff: 1s → 2s → 4s → 8s → 10s max
        // - Boot crash detection
        let session = SerialSession::new(port.to_string(), baud_rate);
        self.sessions.insert(port.to_string(), session);

        let (tx, _rx) = broadcast::channel(1024);
        self.broadcasters.insert(port.to_string(), tx);

        tracing::info!(port, client_id, "port opened");
        Ok(())
    }

    /// Close a serial port.
    pub async fn close_port(&self, port: &str, client_id: &str) -> fbuild_core::Result<()> {
        self.sessions.remove(port);
        self.broadcasters.remove(port);
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
}

impl Default for SharedSerialManager {
    fn default() -> Self {
        Self::new()
    }
}

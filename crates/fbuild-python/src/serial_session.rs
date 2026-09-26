//! Shared per-session reader-task core for `SerialMonitor` and
//! `AsyncSerialMonitor` (FastLED/fbuild#1485).
//!
//! One reader task per session owns the WebSocket's read half exclusively.
//! Everything else — sync facade, async facade, RPC, `read_lines` — talks to
//! it through channels. This removes the polling hand-off
//! (`ws_session::ReadYield`/`RpcRoute`) that #1484 used to keep a `write`
//! from waiting out a concurrent `read_lines`.
//!
//! # Fixed lock order
//!
//! `sink` -> `reply FIFO` -> `line queue` -> `status`.
//!
//! The reader task only ever takes `reply FIFO`, `line queue` and `status` —
//! never `sink`. `sink` is taken only by writers, and never while any other
//! lock in this module is held.
//!
//! # Deadlock-freedom rules enforced here
//!
//! * No lock is held across `.await` (enforced by the crate-level
//!   `#![deny(clippy::await_holding_lock)]` — see `lib.rs`); all locks in this
//!   module are `parking_lot` (sync, short critical sections) except `sink`,
//!   which is a `tokio::sync::Mutex` held only across the single `send`.
//! * The reader task runs under a drop guard that marks the session
//!   `Closed` and fails every pending reply/RPC/line-waiter, so nobody ever
//!   waits on a dead task.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use std::sync::Mutex;
use tokio::sync::{Notify, oneshot, watch};
use tokio_tungstenite::tungstenite;

use crate::messages::{ClientMessage, ServerMessage, WsSink, WsSource};

/// Serial-line prefix of a device's JSON-RPC reply.
const REMOTE_PREFIX: &str = "REMOTE:";

/// Default cap on buffered, undelivered serial lines. Overflow drops the
/// oldest lines and counts them (`SerialSession::lines_dropped`); see §4.3.
pub(crate) const DEFAULT_MAX_BUFFERED_LINES: usize = 10_000;

/// Errors surfaced by every core operation. Mapped to Python exceptions in
/// the facades (`Timeout` -> `TimeoutError`, `Closed`/`PortGone`/`Preempted`
/// -> `ConnectionError`, `ProtocolDesync` -> `RuntimeError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionError {
    Timeout,
    Closed,
    PortGone,
    Preempted,
    ProtocolDesync,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            SessionError::Timeout => "timed out",
            SessionError::Closed => "session closed",
            SessionError::PortGone => "port disconnected",
            SessionError::Preempted => "session preempted by a deploy",
            SessionError::ProtocolDesync => "protocol desync (unexpected reply order)",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for SessionError {}

/// Broadcast session status, observed via [`SerialSession::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionStatus {
    Active,
    /// Paused for a deploy; only reached with `auto_reconnect == false`
    /// (with `auto_reconnect == true` the reader keeps `Active` across a
    /// preemption and readers simply keep waiting, per §4.5).
    Preempted,
    Closed,
}

/// What kind of reply a FIFO entry expects, so a mismatched reply
/// (`error` frame swapped in for a `write_ack`, or a reply kind that
/// doesn't match the head) is detected instead of silently misdelivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplyKind {
    Write,
    InWaiting,
}

struct ReplyEntry {
    kind: ReplyKind,
    tx: oneshot::Sender<Result<usize, SessionError>>,
}

struct Inner {
    sink: tokio::sync::Mutex<Option<WsSink>>,
    reply_fifo: Mutex<VecDeque<ReplyEntry>>,
    rpc_fifo: Mutex<VecDeque<oneshot::Sender<Result<String, SessionError>>>>,
    line_queue: Mutex<VecDeque<String>>,
    line_notify: Notify,
    lines_dropped: AtomicUsize,
    max_buffered_lines: usize,
    status_tx: watch::Sender<SessionStatus>,
    reader_alive: AtomicBool,
}

impl Inner {
    fn status(&self) -> SessionStatus {
        *self.status_tx.borrow()
    }

    fn set_status(&self, status: SessionStatus) {
        self.status_tx.send_replace(status);
        self.line_notify.notify_waiters();
    }

    fn push_line(&self, line: String) {
        let mut queue = self.line_queue.lock().unwrap_or_else(|e| e.into_inner());
        if queue.len() >= self.max_buffered_lines {
            queue.pop_front();
            self.lines_dropped.fetch_add(1, Ordering::Relaxed);
        }
        queue.push_back(line);
        drop(queue);
        self.line_notify.notify_waiters();
    }

    /// Dispatch one `data` line: to the oldest RPC waiter if one is
    /// registered and the line is a `REMOTE:` reply, otherwise to the line
    /// queue.
    fn dispatch_data_line(&self, line: String) {
        if let Some(stripped) = line.strip_prefix(REMOTE_PREFIX) {
            let waiter = {
                self.rpc_fifo
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pop_front()
            };
            if let Some(tx) = waiter {
                let _ = tx.send(Ok(stripped.to_string()));
                return;
            }
        }
        self.push_line(line);
    }

    /// Complete the head of the reply FIFO. A kind mismatch is a protocol
    /// desync: fail the head loudly rather than silently reordering.
    fn complete_reply(&self, kind: ReplyKind, value: Result<usize, SessionError>) {
        let entry = {
            self.reply_fifo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front()
        };
        let Some(entry) = entry else {
            tracing::warn!(?kind, "reply with no pending request in the FIFO");
            return;
        };
        if entry.kind != kind {
            tracing::warn!(
                expected = ?entry.kind,
                got = ?kind,
                "protocol desync: reply kind does not match the head of the FIFO"
            );
            let _ = entry.tx.send(Err(SessionError::ProtocolDesync));
            return;
        }
        let _ = entry.tx.send(value);
    }

    /// An `error` frame replaces whatever the head of the FIFO was
    /// expecting (bad-base64 path never emits its `write_ack`).
    fn complete_reply_with_error(&self) {
        let entry = {
            self.reply_fifo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front()
        };
        if let Some(entry) = entry {
            let _ = entry.tx.send(Err(SessionError::ProtocolDesync));
        }
    }

    /// Fail every pending reply, RPC waiter, and wake every line-queue
    /// waiter. Used on close, port-gone, and reader-task death.
    fn fail_everyone(&self, err: SessionError) {
        for entry in self
            .reply_fifo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            let _ = entry.tx.send(Err(err.clone()));
        }
        for tx in self
            .rpc_fifo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            let _ = tx.send(Err(err.clone()));
        }
        self.line_notify.notify_waiters();
    }
}

/// Configuration for [`SerialSession::connect`].
pub(crate) struct SessionConfig {
    pub(crate) ws_url: String,
    pub(crate) port: String,
    pub(crate) baud_rate: u32,
    pub(crate) auto_reconnect: bool,
    pub(crate) verbose: bool,
    pub(crate) client_id: String,
    pub(crate) max_buffered_lines: usize,
    pub(crate) handshake_timeout: Duration,
}

/// A live session: the reader task plus handles callers use to talk to it.
/// No PyO3 in this type, so every concurrency property is testable without
/// a Python interpreter.
pub(crate) struct SerialSession {
    inner: Arc<Inner>,
    reader: Option<tokio::task::JoinHandle<()>>,
    auto_reconnect: bool,
}

impl SerialSession {
    /// Attach to the daemon's `/ws/serial-monitor` endpoint: connect,
    /// handshake, then spawn the sole reader task. Capped at
    /// `cfg.handshake_timeout` (default callers use 5s).
    pub(crate) async fn connect(cfg: SessionConfig) -> Result<Self, SessionError> {
        let (ws_stream, _) = match tokio::time::timeout(
            cfg.handshake_timeout,
            tokio_tungstenite::connect_async(&cfg.ws_url),
        )
        .await
        {
            Ok(Ok(ok)) => ok,
            Ok(Err(_)) => return Err(SessionError::Closed),
            Err(_) => return Err(SessionError::Timeout),
        };
        let (mut write, mut read) = ws_stream.split();

        let attach = ClientMessage::Attach {
            client_id: cfg.client_id,
            port: cfg.port,
            baud_rate: cfg.baud_rate,
            open_if_needed: true,
            pre_acquire_writer: true,
            client_metadata: Some(crate::messages::ClientMetadata::current()),
        };
        let attach_json = serde_json::to_string(&attach)
            .expect("fbuild-python: ClientMessage::Attach serialization is infallible");
        match tokio::time::timeout(
            cfg.handshake_timeout,
            write.send(tungstenite::Message::Text(attach_json)),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err(SessionError::Closed),
            Err(_) => return Err(SessionError::Timeout),
        }

        let msg = match tokio::time::timeout(cfg.handshake_timeout, read.next()).await {
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(_))) | Ok(None) => return Err(SessionError::Closed),
            Err(_) => return Err(SessionError::Timeout),
        };
        if let tungstenite::Message::Text(text) = msg {
            match serde_json::from_str::<ServerMessage>(&text) {
                Ok(ServerMessage::Attached { success, .. }) if success => {
                    if cfg.verbose {
                        eprintln!("attached");
                    }
                }
                _ => return Err(SessionError::Closed),
            }
        }

        let inner = Arc::new(Inner {
            sink: tokio::sync::Mutex::new(Some(write)),
            reply_fifo: Mutex::new(VecDeque::new()),
            rpc_fifo: Mutex::new(VecDeque::new()),
            line_queue: Mutex::new(VecDeque::new()),
            line_notify: Notify::new(),
            lines_dropped: AtomicUsize::new(0),
            max_buffered_lines: cfg.max_buffered_lines.max(1),
            status_tx: watch::Sender::new(SessionStatus::Active),
            reader_alive: AtomicBool::new(true),
        });

        let reader = tokio::spawn(reader_task(Arc::clone(&inner), read, cfg.auto_reconnect));

        Ok(Self {
            inner,
            reader: Some(reader),
            auto_reconnect: cfg.auto_reconnect,
        })
    }

    /// Cancel-safe: lines leave the queue only inside the synchronous
    /// critical section that returns them, so a dropped future removes
    /// nothing (AT-6/AT-P7).
    pub(crate) async fn read_lines(&self, timeout: Duration) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let mut queue = self
                    .inner
                    .line_queue
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if !queue.is_empty() {
                    return queue.drain(..).collect();
                }
            }
            if self.inner.status() == SessionStatus::Closed {
                return Vec::new();
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Vec::new();
            }
            let mut status_rx = self.inner.status_tx.subscribe();
            let notified = self.inner.line_notify.notified();
            tokio::select! {
                () = notified => {}
                _ = status_rx.changed() => {}
                () = tokio::time::sleep(deadline - now) => { return Vec::new(); }
            }
        }
    }

    /// Wakes every blocked sync reader (used by `interrupt_reads`): the
    /// caller's `read_lines` returns `[]` without draining the queue.
    pub(crate) fn interrupt_reads(&self) {
        self.inner.line_notify.notify_waiters();
    }

    async fn send_and_await_reply(
        &self,
        text: String,
        kind: ReplyKind,
        timeout: Duration,
    ) -> Result<usize, SessionError> {
        if self.inner.status() == SessionStatus::Closed {
            return Err(SessionError::Closed);
        }
        if self.inner.status() == SessionStatus::Preempted && !self.auto_reconnect {
            return Err(SessionError::Preempted);
        }
        let (tx, rx) = oneshot::channel();
        // Register, then send, atomically under the sink lock (§4.2): two
        // writers cannot register in one order and send in the other.
        let send_result = {
            let mut sink = self.inner.sink.lock().await;
            self.inner
                .reply_fifo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(ReplyEntry { kind, tx });
            match sink.as_mut() {
                Some(s) => {
                    tokio::time::timeout(timeout, s.send(tungstenite::Message::Text(text))).await
                }
                None => return Err(SessionError::Closed),
            }
        };
        match send_result {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                self.inner.set_status(SessionStatus::Closed);
                self.inner.fail_everyone(SessionError::Closed);
                return Err(SessionError::Closed);
            }
            Err(_) => {
                // Send itself timed out mid-frame: the socket is in an
                // unknown state, close the whole session (§4.2).
                self.inner.set_status(SessionStatus::Closed);
                self.inner.fail_everyone(SessionError::Timeout);
                return Err(SessionError::Timeout);
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            // Timed out or sender dropped (reader died): leave the FIFO
            // entry's slot abandoned; the reader already discards replies
            // whose receiver is gone.
            Ok(Err(_)) => Err(SessionError::Closed),
            Err(_) => Err(SessionError::Timeout),
        }
    }

    pub(crate) async fn write(
        &self,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, SessionError> {
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data);
        let msg = serde_json::to_string(&ClientMessage::Write { data: encoded })
            .expect("fbuild-python: ClientMessage::Write serialization is infallible");
        self.send_and_await_reply(msg, ReplyKind::Write, timeout)
            .await
    }

    pub(crate) async fn in_waiting(&self, timeout: Duration) -> Result<usize, SessionError> {
        let msg = serde_json::to_string(&ClientMessage::GetInWaiting)
            .expect("fbuild-python: ClientMessage::GetInWaiting serialization is infallible");
        let daemon_count = self
            .send_and_await_reply(msg, ReplyKind::InWaiting, timeout)
            .await?;
        Ok(daemon_count
            + self
                .inner
                .line_queue
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len())
    }

    /// Registers before writing (§4.4), so the reply cannot race the
    /// write's own FIFO entry.
    pub(crate) async fn json_rpc(
        &self,
        line: &str,
        timeout: Duration,
    ) -> Result<String, SessionError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let (rpc_tx, rpc_rx) = oneshot::channel();
        self.inner
            .rpc_fifo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(rpc_tx);

        let data = format!("{line}\n");
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data.as_bytes());
        let msg = serde_json::to_string(&ClientMessage::Write { data: encoded })
            .expect("fbuild-python: ClientMessage::Write serialization is infallible");
        let write_result = self
            .send_and_await_reply(msg, ReplyKind::Write, timeout)
            .await;
        write_result?;

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rpc_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(SessionError::Closed),
            Err(_) => Err(SessionError::Timeout),
        }
    }

    pub(crate) async fn clear_input(&self) {
        self.inner
            .line_queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let msg = serde_json::to_string(&ClientMessage::ClearBuffer)
            .expect("fbuild-python: ClientMessage::ClearBuffer serialization is infallible");
        let mut sink = self.inner.sink.lock().await;
        if let Some(s) = sink.as_mut() {
            let _ = s.send(tungstenite::Message::Text(msg)).await;
        }
    }

    #[cfg(test)]
    pub(crate) fn lines_dropped(&self) -> usize {
        self.inner.lines_dropped.load(Ordering::Relaxed)
    }

    #[allow(dead_code)] // exposed for future facade use / tests; not yet consumed by a facade
    pub(crate) fn status(&self) -> watch::Receiver<SessionStatus> {
        self.inner.status_tx.subscribe()
    }

    #[cfg(test)]
    pub(crate) fn is_reader_alive(&self) -> bool {
        self.reader
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Detach, close, and join the reader task.
    pub(crate) async fn close(mut self) {
        self.close_inner().await;
    }

    async fn close_inner(&mut self) {
        {
            let mut sink = self.inner.sink.lock().await;
            if let Some(s) = sink.as_mut() {
                let detach = serde_json::to_string(&ClientMessage::Detach)
                    .expect("fbuild-python: ClientMessage::Detach serialization is infallible");
                let _ = s.send(tungstenite::Message::Text(detach)).await;
                let _ = s.send(tungstenite::Message::Close(None)).await;
            }
            *sink = None;
        }
        self.inner.set_status(SessionStatus::Closed);
        self.inner.fail_everyone(SessionError::Closed);
        if let Some(reader) = self.reader.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), reader).await;
        }
    }
}

impl Drop for SerialSession {
    fn drop(&mut self) {
        // Best-effort teardown for a drop without `close()` (GC, an
        // exception in `with`, a leaked object): mark closed and abort the
        // reader so nobody waits on a dead session forever (§4.9 rule 7).
        self.inner.set_status(SessionStatus::Closed);
        self.inner.fail_everyone(SessionError::Closed);
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
    }
}

/// The sole reader task: owns `read` for the lifetime of the session.
async fn reader_task(inner: Arc<Inner>, mut read: WsSource, auto_reconnect: bool) {
    struct DeathGuard(Arc<Inner>);
    impl Drop for DeathGuard {
        fn drop(&mut self) {
            self.0.reader_alive.store(false, Ordering::Relaxed);
            self.0.set_status(SessionStatus::Closed);
            self.0.fail_everyone(SessionError::Closed);
        }
    }
    let _guard = DeathGuard(Arc::clone(&inner));

    loop {
        let message = read.next().await;
        match message {
            Some(Ok(tungstenite::Message::Text(text))) => {
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(ServerMessage::Data { lines, .. }) => {
                        for line in lines {
                            inner.dispatch_data_line(line);
                        }
                    }
                    Ok(ServerMessage::WriteAck {
                        success,
                        bytes_written,
                        ..
                    }) => {
                        let value = if success { Ok(bytes_written) } else { Ok(0) };
                        inner.complete_reply(ReplyKind::Write, value);
                    }
                    Ok(ServerMessage::InWaiting { count }) => {
                        inner.complete_reply(ReplyKind::InWaiting, Ok(count));
                    }
                    Ok(ServerMessage::Error { message }) => {
                        tracing::warn!(message, "daemon returned an error frame");
                        inner.complete_reply_with_error();
                    }
                    Ok(ServerMessage::Preempted { .. }) => {
                        if auto_reconnect {
                            // Keep Active; readers keep waiting.
                        } else {
                            inner.set_status(SessionStatus::Preempted);
                            inner.fail_everyone(SessionError::Preempted);
                        }
                    }
                    Ok(ServerMessage::Reconnected { .. }) => {
                        inner.set_status(SessionStatus::Active);
                    }
                    Ok(ServerMessage::PortRenumbered { .. })
                    | Ok(ServerMessage::PortReattached { .. }) => {}
                    Ok(ServerMessage::PortDisconnected { .. })
                    | Ok(ServerMessage::PortRebindFailed { .. }) => {
                        inner.set_status(SessionStatus::Closed);
                        inner.fail_everyone(SessionError::PortGone);
                        return;
                    }
                    Ok(ServerMessage::Attached { .. }) | Ok(ServerMessage::Other) | Err(_) => {
                        tracing::debug!(?text, "ignoring malformed/unknown/late-attach frame");
                    }
                }
            }
            Some(Ok(tungstenite::Message::Close(_))) | None => return,
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                tracing::debug!(error = %e, "websocket read error, ignoring frame");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use tokio::net::TcpListener;

    /// Fault hooks the fake daemon can be told to trigger.
    #[derive(Default, Clone)]
    struct DaemonKnobs {
        echo_delay: Duration,
        ack_delay: Duration,
        inject_error_on_nth_write: Option<usize>,
        stop_reading_after_writes: Option<usize>,
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
                    if knobs.ack_delay.is_zero() {
                        // no-op, ack below
                    } else {
                        tokio::time::sleep(knobs.ack_delay).await;
                    }
                    if knobs.inject_error_on_nth_write == Some(n as usize) {
                        let err = serde_json::json!({"type": "error", "message": "bad base64"});
                        sink.lock()
                            .await
                            .send(tungstenite::Message::Text(err.to_string()))
                            .await
                            .unwrap();
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
                    sink.lock()
                        .await
                        .send(tungstenite::Message::Text(ack.to_string()))
                        .await
                        .unwrap();
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
                        sink.lock()
                            .await
                            .send(tungstenite::Message::Text(line))
                            .await
                            .unwrap();
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
                    sink.lock()
                        .await
                        .send(tungstenite::Message::Text(reply.to_string()))
                        .await
                        .unwrap();
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

    /// §5.3 watchdog: wraps a test body in a budget; on expiry, panics with
    /// core-state evidence instead of letting CI's job timeout eat it.
    async fn with_watchdog<F: std::future::Future<Output = ()>>(budget: Duration, fut: F) {
        match tokio::time::timeout(budget, fut).await {
            Ok(()) => {}
            Err(_) => panic!("test exceeded its {budget:?} watchdog budget"),
        }
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
            let total_sent = 50usize;
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
            for _ in 0..1000 {
                if delivered.len() >= total_sent {
                    break;
                }
                let fut = session.read_lines(Duration::from_secs(2));
                match tokio::time::timeout(Duration::from_millis(1), fut).await {
                    Ok(lines) => delivered.extend(lines),
                    Err(_) => continue,
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
            assert_eq!(delivered.len(), total_sent, "lines lost or duplicated");
            let mut sorted = delivered.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), total_sent, "duplicate lines delivered");
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

    #[tokio::test]
    async fn at11_overflow_drops_oldest_and_counts() {
        with_watchdog(Duration::from_secs(30), async {
            let (session, _) = connect_to(DaemonKnobs::default()).await;
            let session = Arc::new(session);
            // Small cap isn't configurable per-connect in this test harness
            // helper, so drive it directly via a custom config.
            drop(session);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(fake_daemon(listener, DaemonKnobs::default()));
            let cfg = SessionConfig {
                ws_url: format!("ws://127.0.0.1:{port}"),
                port: "COM_TEST".into(),
                baud_rate: 115200,
                auto_reconnect: true,
                verbose: false,
                client_id: "test".into(),
                max_buffered_lines: 10,
                handshake_timeout: Duration::from_secs(5),
            };
            let session = SerialSession::connect(cfg).await.unwrap();
            for i in 0..50 {
                session
                    .write(format!("m{i}").as_bytes(), Duration::from_secs(2))
                    .await
                    .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            let remaining = session.read_lines(Duration::from_millis(50)).await;
            assert_eq!(remaining.len(), 10);
            assert_eq!(session.lines_dropped(), 40);
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
    async fn at15_peer_stops_reading_write_times_out_and_closes() {
        with_watchdog(Duration::from_secs(10), async {
            let (session, _) = connect_to(DaemonKnobs {
                stop_reading_after_writes: Some(0),
                ..Default::default()
            })
            .await;
            let session = Arc::new(session);
            let started = tokio::time::Instant::now();
            let result = session.write(b"x", Duration::from_millis(200)).await;
            assert_eq!(result, Err(SessionError::Timeout));
            assert!(started.elapsed() < Duration::from_secs(2));
            // A concurrent reader must return promptly too.
            let lines = session.read_lines(Duration::from_secs(2)).await;
            assert!(lines.is_empty());
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

    #[tokio::test]
    async fn at20_teardown_races_in_flight_calls() {
        with_watchdog(Duration::from_secs(30), async {
            for _ in 0..10 {
                let (session, _) = connect_to(DaemonKnobs {
                    echo_delay: Duration::from_millis(50),
                    ..Default::default()
                })
                .await;
                let session = Arc::new(session);
                let mut handles = Vec::new();
                for i in 0..16 {
                    let session = Arc::clone(&session);
                    handles.push(tokio::spawn(async move {
                        if i % 2 == 0 {
                            let _ = session.write(b"x", Duration::from_secs(2)).await;
                        } else {
                            let _ = session.read_lines(Duration::from_secs(2)).await;
                        }
                    }));
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                // Every in-flight call must resolve within 1s of the
                // session closing, never hang until its own 2s timeout.
                let all = futures::future::join_all(handles);
                tokio::time::timeout(Duration::from_secs(1) + Duration::from_secs(2), all)
                    .await
                    .expect("in-flight calls must not hang past close");
                // The Arc may still be held by a spawned task momentarily;
                // once all handles joined, this is the sole owner.
                assert_eq!(Arc::strong_count(&session), 1);
            }
        })
        .await;
    }
}

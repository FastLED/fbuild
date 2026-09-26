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
//! `sink` -> `request gate` -> `reply FIFO` -> `line queue` -> `status`.
//!
//! The reader task only ever takes the request gate, reply FIFO, line queue
//! and status — never `sink`. Writers take `sink` before the request gate,
//! and drop the gate before awaiting the WebSocket send.
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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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

/// How many `SerialSession`s are currently live (incremented on a
/// successful `connect`, decremented in `Drop`). Compiled only in debug
/// builds (on for `cargo test`, off for a release wheel): AT-P16 uses this,
/// via `crate::live_session_count()`, to prove a dropped-without-`close()`
/// session's reader task actually terminates instead of leaking.
#[cfg(debug_assertions)]
static LIVE_SESSIONS: AtomicUsize = AtomicUsize::new(0);

#[cfg(debug_assertions)]
pub(crate) fn live_session_count() -> usize {
    LIVE_SESSIONS.load(Ordering::SeqCst)
}

/// Errors surfaced by every core operation. Mapped to Python exceptions in
/// the facades (`Timeout` -> `TimeoutError`, `Closed`/`PortGone`/`Preempted`/`ConnectionFailed`
/// -> `ConnectionError`, `ProtocolDesync`/`WriteFailed` -> `RuntimeError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionError {
    Timeout,
    Closed,
    ConnectionFailed(String),
    PortGone,
    Preempted,
    ProtocolDesync,
    WriteFailed(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            SessionError::Timeout => "timed out",
            SessionError::Closed => "session closed",
            SessionError::ConnectionFailed(message) => return f.write_str(message),
            SessionError::PortGone => "port disconnected",
            SessionError::Preempted => "session preempted by a deploy",
            SessionError::ProtocolDesync => "protocol desync (unexpected reply order)",
            SessionError::WriteFailed(message) => {
                return write!(f, "serial write failed: {message}");
            }
        };
        f.write_str(msg)
    }
}

impl std::error::Error for SessionError {}

/// Broadcast session status, observed via [`SerialSession::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionStatus {
    Active,
    /// Paused for a deploy. Readers keep waiting when auto-reconnect is on;
    /// writes fail promptly in either mode.
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
    tx: Option<oneshot::Sender<Result<usize, SessionError>>>,
    rpc_id: Option<u64>,
}

struct RpcEntry {
    id: u64,
    tx: Option<oneshot::Sender<Result<String, SessionError>>>,
}

struct Inner {
    sink: tokio::sync::Mutex<Option<WsSink>>,
    /// Serializes status checks plus FIFO registration with terminal/preempt
    /// sweeps. Never held across a WebSocket send or any other `.await`.
    request_gate: Mutex<()>,
    reply_fifo: Mutex<VecDeque<ReplyEntry>>,
    rpc_fifo: Mutex<VecDeque<RpcEntry>>,
    next_rpc_id: AtomicU64,
    line_queue: Mutex<VecDeque<String>>,
    line_notify: Notify,
    read_interrupt_epoch: AtomicUsize,
    lines_dropped: AtomicUsize,
    #[cfg(test)]
    lines_received_for_test: AtomicUsize,
    #[cfg(test)]
    lines_delivered_for_test: AtomicUsize,
    #[cfg(test)]
    lines_discarded_for_test: AtomicUsize,
    overflow_warned: AtomicBool,
    max_buffered_lines: usize,
    status_tx: watch::Sender<SessionStatus>,
    reader_alive: AtomicBool,
    /// The reader task's handle, behind a lock so `close()` can take `&self`
    /// (needed for AT-20: closing a shared `Arc<SerialSession>` while other
    /// holders have calls in flight).
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Inner {
    fn status(&self) -> SessionStatus {
        *self.status_tx.borrow()
    }

    fn set_status(&self, status: SessionStatus) {
        self.status_tx.send_if_modified(|current| {
            if *current == SessionStatus::Closed && status != SessionStatus::Closed {
                return false;
            }
            *current = status;
            true
        });
        self.line_notify.notify_waiters();
    }

    fn reconnect_if_preempted(&self) {
        self.status_tx.send_if_modified(|current| {
            if *current != SessionStatus::Preempted {
                return false;
            }
            *current = SessionStatus::Active;
            true
        });
        self.line_notify.notify_waiters();
    }

    fn push_line(&self, line: String) {
        let mut queue = self.line_queue.lock().unwrap_or_else(|e| e.into_inner());
        if queue.len() >= self.max_buffered_lines {
            queue.pop_front();
            let dropped = self.lines_dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if !self.overflow_warned.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    dropped,
                    max_buffered_lines = self.max_buffered_lines,
                    "serial line queue overflow; dropping oldest lines"
                );
            }
        }
        queue.push_back(line);
        drop(queue);
        self.line_notify.notify_waiters();
    }

    /// Dispatch one `data` line: to the oldest RPC waiter if one is
    /// registered and the line is a `REMOTE:` reply, otherwise to the line
    /// queue.
    fn dispatch_data_line(&self, line: String) {
        #[cfg(test)]
        self.lines_received_for_test.fetch_add(1, Ordering::Relaxed);
        if let Some(stripped) = line.strip_prefix(REMOTE_PREFIX) {
            let waiter = {
                self.rpc_fifo
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pop_front()
            };
            if let Some(entry) = waiter {
                let delivered = entry
                    .tx
                    .is_some_and(|tx| tx.send(Ok(stripped.to_string())).is_ok());
                #[cfg(test)]
                if delivered {
                    self.lines_delivered_for_test
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    self.lines_discarded_for_test
                        .fetch_add(1, Ordering::Relaxed);
                }
                #[cfg(not(test))]
                let _ = delivered;
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
            if let Some(tx) = entry.tx {
                let _ = tx.send(Err(SessionError::ProtocolDesync));
            }
            return;
        }
        if value.is_err() {
            self.remove_rpc(entry.rpc_id);
        }
        if let Some(tx) = entry.tx {
            let _ = tx.send(value);
        }
    }

    fn remove_rpc(&self, rpc_id: Option<u64>) {
        if let Some(id) = rpc_id {
            self.rpc_fifo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|entry| entry.id != id);
        }
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
            self.remove_rpc(entry.rpc_id);
            if let Some(tx) = entry.tx {
                let _ = tx.send(Err(SessionError::ProtocolDesync));
            }
        }
    }

    /// Fail callers immediately, but keep their FIFO slots so replies already
    /// in flight cannot be mistaken for replies to post-reconnect requests.
    fn preempt_everyone(&self) {
        let _request_gate = self.request_gate.lock().unwrap_or_else(|e| e.into_inner());
        for entry in self
            .reply_fifo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter_mut()
        {
            if let Some(tx) = entry.tx.take() {
                let _ = tx.send(Err(SessionError::Preempted));
            }
        }
        for entry in self
            .rpc_fifo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter_mut()
        {
            if let Some(tx) = entry.tx.take() {
                let _ = tx.send(Err(SessionError::Preempted));
            }
        }
        self.line_notify.notify_waiters();
    }

    /// Fail every pending reply, RPC waiter, and wake every line-queue
    /// waiter. Used on close, port-gone, and reader-task death.
    fn fail_everyone(&self, err: SessionError) {
        let _request_gate = self.request_gate.lock().unwrap_or_else(|e| e.into_inner());
        for entry in self
            .reply_fifo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            if let Some(tx) = entry.tx {
                let _ = tx.send(Err(err.clone()));
            }
        }
        for entry in self
            .rpc_fifo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            if let Some(tx) = entry.tx {
                let _ = tx.send(Err(err.clone()));
            }
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
    auto_reconnect: bool,
}

impl SerialSession {
    /// Attach to the daemon's `/ws/serial-monitor` endpoint: connect,
    /// handshake, then spawn the sole reader task. Capped at
    /// `cfg.handshake_timeout` (default callers use 5s).
    pub(crate) async fn connect(cfg: SessionConfig) -> Result<Self, SessionError> {
        let deadline = tokio::time::Instant::now() + cfg.handshake_timeout;
        let (ws_stream, _) =
            match tokio::time::timeout_at(deadline, tokio_tungstenite::connect_async(&cfg.ws_url))
                .await
            {
                Ok(Ok(ok)) => ok,
                Ok(Err(error)) => {
                    return Err(SessionError::ConnectionFailed(format!(
                        "failed to connect to daemon WebSocket at {}: {error}",
                        cfg.ws_url
                    )));
                }
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
        match tokio::time::timeout_at(
            deadline,
            write.send(tungstenite::Message::Text(attach_json)),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err(SessionError::Closed),
            Err(_) => return Err(SessionError::Timeout),
        }

        let msg = match tokio::time::timeout_at(deadline, read.next()).await {
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(_))) | Ok(None) => return Err(SessionError::Closed),
            Err(_) => return Err(SessionError::Timeout),
        };
        match msg {
            tungstenite::Message::Text(text) => {
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(ServerMessage::Attached { success, .. }) if success => {
                        if cfg.verbose {
                            eprintln!("attached");
                        }
                    }
                    _ => return Err(SessionError::Closed),
                }
            }
            _ => return Err(SessionError::Closed),
        }

        let inner = Arc::new(Inner {
            sink: tokio::sync::Mutex::new(Some(write)),
            request_gate: Mutex::new(()),
            reply_fifo: Mutex::new(VecDeque::new()),
            rpc_fifo: Mutex::new(VecDeque::new()),
            next_rpc_id: AtomicU64::new(0),
            line_queue: Mutex::new(VecDeque::new()),
            line_notify: Notify::new(),
            read_interrupt_epoch: AtomicUsize::new(0),
            lines_dropped: AtomicUsize::new(0),
            #[cfg(test)]
            lines_received_for_test: AtomicUsize::new(0),
            #[cfg(test)]
            lines_delivered_for_test: AtomicUsize::new(0),
            #[cfg(test)]
            lines_discarded_for_test: AtomicUsize::new(0),
            overflow_warned: AtomicBool::new(false),
            max_buffered_lines: cfg.max_buffered_lines.max(1),
            status_tx: watch::Sender::new(SessionStatus::Active),
            reader_alive: AtomicBool::new(true),
            reader: Mutex::new(None),
        });

        let reader = tokio::spawn(reader_task(Arc::clone(&inner), read, cfg.auto_reconnect));
        *inner.reader.lock().unwrap_or_else(|e| e.into_inner()) = Some(reader);

        #[cfg(debug_assertions)]
        LIVE_SESSIONS.fetch_add(1, Ordering::SeqCst);

        Ok(Self {
            inner,
            auto_reconnect: cfg.auto_reconnect,
        })
    }

    /// Cancel-safe: lines leave the queue only inside the synchronous
    /// critical section that returns them, so a dropped future removes
    /// nothing (AT-6/AT-P7).
    pub(crate) async fn read_lines(&self, timeout: Duration) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + timeout;
        let interrupt_epoch = self.inner.read_interrupt_epoch.load(Ordering::Acquire);
        loop {
            // Register for notification before inspecting the queue. This
            // closes the gap where a line (or interrupt) could arrive after
            // the inspection but before the waiter was registered.
            let notified = self.inner.line_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut status_rx = self.inner.status_tx.subscribe();
            {
                let mut queue = self
                    .inner
                    .line_queue
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                // Interruption and draining share this lock: an interrupt
                // that wins the race must leave the queued lines untouched
                // for the next reader (FastLED #3219).
                if self.inner.read_interrupt_epoch.load(Ordering::Acquire) != interrupt_epoch {
                    return Vec::new();
                }
                if !queue.is_empty() {
                    let lines: Vec<String> = queue.drain(..).collect();
                    #[cfg(test)]
                    self.inner
                        .lines_delivered_for_test
                        .fetch_add(lines.len(), Ordering::Relaxed);
                    self.inner.overflow_warned.store(false, Ordering::Relaxed);
                    return lines;
                }
            }
            if self.inner.status() == SessionStatus::Closed
                || (self.inner.status() == SessionStatus::Preempted && !self.auto_reconnect)
            {
                return Vec::new();
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Vec::new();
            }
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
        let queue_guard = self
            .inner
            .line_queue
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.inner
            .read_interrupt_epoch
            .fetch_add(1, Ordering::AcqRel);
        drop(queue_guard);
        self.inner.line_notify.notify_waiters();
    }

    /// Restore a batch when Python cancels after the core read resolved but
    /// before the async bridge delivered it. The core read future itself is
    /// cancel-safe; this closes the bridge's separate delivery window.
    pub(crate) fn restore_cancelled_read(&self, lines: Vec<String>) {
        if lines.is_empty() {
            return;
        }
        #[cfg(test)]
        self.inner
            .lines_delivered_for_test
            .fetch_sub(lines.len(), Ordering::Relaxed);
        let mut queue = self
            .inner
            .line_queue
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for line in lines.into_iter().rev() {
            if queue.len() >= self.inner.max_buffered_lines {
                queue.pop_back();
                self.inner.lines_dropped.fetch_add(1, Ordering::Relaxed);
            }
            queue.push_front(line);
        }
        drop(queue);
        self.inner.line_notify.notify_waiters();
    }

    async fn send_and_await_reply(
        &self,
        text: String,
        kind: ReplyKind,
        timeout: Duration,
    ) -> Result<usize, SessionError> {
        self.send_and_await_reply_with_rpc_waiter(text, kind, timeout, None)
            .await
    }

    /// Like [`Self::send_and_await_reply`], but when `rpc_waiter` is
    /// `Some`, registers it on the RPC-reply FIFO in the **same** critical
    /// section as the reply-FIFO entry and the send itself. This is what
    /// [`Self::json_rpc`] needs: registering the RPC waiter before taking
    /// the sink lock (as an earlier version of this code did) let two
    /// concurrent `json_rpc` calls register their RPC waiters in one order
    /// but send in the other, so a reply could be delivered to the wrong
    /// caller (FIFO order must match wire order exactly).
    async fn send_and_await_reply_with_rpc_waiter(
        &self,
        text: String,
        kind: ReplyKind,
        timeout: Duration,
        rpc_waiter: Option<oneshot::Sender<Result<String, SessionError>>>,
    ) -> Result<usize, SessionError> {
        let deadline = tokio::time::Instant::now() + timeout;
        if self.inner.status() == SessionStatus::Closed {
            return Err(SessionError::Closed);
        }
        if self.inner.status() == SessionStatus::Preempted {
            return Err(SessionError::Preempted);
        }
        let (tx, rx) = oneshot::channel();
        // Register (both the write-reply FIFO and, if present, the RPC
        // waiter), then send, atomically under the sink lock (§4.2/§4.4):
        // two callers cannot register in one order and send in the other.
        let send_result = {
            let mut sink = tokio::time::timeout_at(deadline, self.inner.sink.lock())
                .await
                .map_err(|_| SessionError::Timeout)?;
            // close() can run while this caller waits for the sink.
            let _request_gate = self
                .inner
                .request_gate
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.inner.status() == SessionStatus::Closed {
                return Err(SessionError::Closed);
            }
            if self.inner.status() == SessionStatus::Preempted {
                return Err(SessionError::Preempted);
            }
            let rpc_id = rpc_waiter
                .as_ref()
                .map(|_| self.inner.next_rpc_id.fetch_add(1, Ordering::Relaxed));
            self.inner
                .reply_fifo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(ReplyEntry {
                    kind,
                    tx: Some(tx),
                    rpc_id,
                });
            if let Some(rpc_waiter) = rpc_waiter {
                self.inner
                    .rpc_fifo
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push_back(RpcEntry {
                        id: rpc_id.expect("RPC waiter has an ID"),
                        tx: Some(rpc_waiter),
                    });
            }
            drop(_request_gate);
            match sink.as_mut() {
                Some(s) => {
                    tokio::time::timeout_at(deadline, s.send(tungstenite::Message::Text(text)))
                        .await
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
        match tokio::time::timeout_at(deadline, rx).await {
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

    /// Registers the RPC waiter atomically with the write's own reply-FIFO
    /// entry and the send itself (§4.4): registering the RPC waiter as a
    /// separate, earlier step let two concurrent `json_rpc` calls register
    /// in one order but reach the sink lock (and so hit the wire) in the
    /// other, so a reply could resolve the wrong caller's waiter.
    pub(crate) async fn json_rpc(
        &self,
        line: &str,
        timeout: Duration,
    ) -> Result<String, SessionError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let (rpc_tx, rpc_rx) = oneshot::channel();

        let data = format!("{line}\n");
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data.as_bytes());
        let msg = serde_json::to_string(&ClientMessage::Write { data: encoded })
            .expect("fbuild-python: ClientMessage::Write serialization is infallible");
        self.send_and_await_reply_with_rpc_waiter(msg, ReplyKind::Write, timeout, Some(rpc_tx))
            .await?;

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rpc_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(SessionError::Closed),
            Err(_) => Err(SessionError::Timeout),
        }
    }

    pub(crate) async fn clear_input(&self) {
        {
            let mut queue = self
                .inner
                .line_queue
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            #[cfg(test)]
            self.inner
                .lines_discarded_for_test
                .fetch_add(queue.len(), Ordering::Relaxed);
            queue.clear();
        }
        self.inner.overflow_warned.store(false, Ordering::Relaxed);
        let msg = serde_json::to_string(&ClientMessage::ClearBuffer)
            .expect("fbuild-python: ClientMessage::ClearBuffer serialization is infallible");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let Ok(mut sink) = tokio::time::timeout_at(deadline, self.inner.sink.lock()).await else {
            return;
        };
        if self.inner.status() == SessionStatus::Closed {
            return;
        }
        if let Some(s) = sink.as_mut() {
            if !matches!(
                tokio::time::timeout_at(deadline, s.send(tungstenite::Message::Text(msg))).await,
                Ok(Ok(()))
            ) {
                // A cancelled WebSocket send may have left a partial frame.
                self.inner.set_status(SessionStatus::Closed);
                self.inner.fail_everyone(SessionError::Closed);
            }
        }
    }

    pub(crate) fn lines_dropped(&self) -> usize {
        self.inner.lines_dropped.load(Ordering::Relaxed)
    }

    #[allow(dead_code)] // exposed for future facade use / tests; not yet consumed by a facade
    pub(crate) fn status(&self) -> watch::Receiver<SessionStatus> {
        self.inner.status_tx.subscribe()
    }

    #[cfg(test)]
    pub(crate) fn is_reader_alive(&self) -> bool {
        self.inner
            .reader
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Detach, mark closed, fail every pending call, and join the reader
    /// task. Takes `&self` (not ownership) so a session shared behind an
    /// `Arc<SerialSession>` can be closed while other holders have calls in
    /// flight (AT-20): those calls observe `Closed` and return promptly
    /// instead of hanging until their own timeout. Idempotent — a second
    /// call (or `Drop` running afterwards) finds the reader handle already
    /// taken and is a no-op beyond re-marking `Closed`.
    pub(crate) async fn close(&self) {
        self.inner.set_status(SessionStatus::Closed);
        self.inner.fail_everyone(SessionError::Closed);
        // Best-effort detach/close notice to the daemon, bounded so a peer
        // that isn't reading (e.g. busy handling an earlier request) can't
        // make `close()` — and so `__aexit__` — hang (AT-P15).
        if let Ok(mut sink) =
            tokio::time::timeout(Duration::from_millis(200), self.inner.sink.lock()).await
        {
            if let Some(s) = sink.as_mut() {
                let detach = serde_json::to_string(&ClientMessage::Detach)
                    .expect("fbuild-python: ClientMessage::Detach serialization is infallible");
                let _ = tokio::time::timeout(Duration::from_millis(200), async {
                    let _ = s.send(tungstenite::Message::Text(detach)).await;
                    s.send(tungstenite::Message::Close(None)).await
                })
                .await;
            }
            *sink = None;
        }
        // Abort the reader immediately rather than waiting for it to
        // notice the peer closing: application-level cleanup is already
        // done above (every pending call has been failed), so `close()`
        // must not depend on the peer's responsiveness to complete.
        let reader = self
            .inner
            .reader
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(reader) = reader {
            reader.abort();
            let _ = reader.await;
        }
    }
}

impl Drop for SerialSession {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        LIVE_SESSIONS.fetch_sub(1, Ordering::SeqCst);
        // Best-effort teardown for a drop without `close()` (GC, an
        // exception in `with`, a leaked object): mark closed and abort the
        // reader so nobody waits on a dead session forever (§4.9 rule 7).
        // A prior `close()` already took the reader handle, so this is a
        // no-op in that case beyond re-marking `Closed`.
        self.inner.set_status(SessionStatus::Closed);
        self.inner.fail_everyone(SessionError::Closed);
        let reader = self
            .inner
            .reader
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(reader) = reader {
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
                #[cfg(test)]
                if text == "__fbuild_test_reader_panic__" {
                    panic!("injected reader-task failure for AT-17");
                }
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(ServerMessage::Data { lines, .. }) => {
                        for line in lines {
                            inner.dispatch_data_line(line);
                        }
                    }
                    Ok(ServerMessage::WriteAck {
                        success,
                        bytes_written,
                        message,
                    }) => {
                        let value = if success {
                            Ok(bytes_written)
                        } else {
                            Err(SessionError::WriteFailed(
                                message.unwrap_or_else(|| "daemon rejected write".to_string()),
                            ))
                        };
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
                        inner.set_status(SessionStatus::Preempted);
                        inner.preempt_everyone();
                    }
                    Ok(ServerMessage::Reconnected { .. }) => {
                        if auto_reconnect {
                            inner.reconnect_if_preempted();
                        }
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
                tracing::warn!(error = %e, "websocket read failed; closing serial session");
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "serial_session/tests.rs"]
mod tests;

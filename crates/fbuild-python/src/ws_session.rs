//! WebSocket session plumbing for the sync `SerialMonitor`.
//!
//! One WebSocket carries both serial `data` frames and the typed replies to
//! requests (`write_ack`, `in_waiting`). Readers and request/reply callers
//! therefore share the read half, behind `read`.
//!
//! FastLED/fbuild#1431: a reader blocked on the read half for its whole
//! timeout made every `write` wait out that timeout before it could collect
//! its `write_ack`, so callers doing request/reply traffic paid the read
//! timeout on every request. A request/reply caller now announces itself
//! through [`ReadYield`]; a waiting reader gives up the read half at once,
//! and resumes when the caller has its reply. Serial lines that arrive while
//! a request/reply caller holds the read half go to `pending`, so no line is
//! lost or reordered.
//!
//! Request/reply calls are serialized with each other (`requests`), so two
//! overlapping calls cannot consume each other's replies. While a
//! `write_json_rpc` waits ([`RpcRoute`]), every reader routes `REMOTE:` reply
//! lines to it instead of returning them to a `read_lines` caller.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::runtime::Runtime;
use tokio_tungstenite::tungstenite;

use crate::messages::{ServerMessage, WsSink, WsSource};

/// How often a reader that yielded re-checks whether the read half is free
/// again. Request/reply callers hold it only until their reply arrives.
const YIELD_POLL: Duration = Duration::from_millis(1);

/// Longest a `write_json_rpc` waiter holds the read half before re-checking
/// its reply queue.
const RPC_SLICE: Duration = Duration::from_millis(50);

/// Serial-line prefix of a device's JSON-RPC reply.
const REMOTE_PREFIX: &str = "REMOTE:";

/// Lets request/reply callers take the read half away from a waiting reader.
#[derive(Default)]
pub(crate) struct ReadYield {
    waiters: AtomicUsize,
    notify: tokio::sync::Notify,
}

/// Held by a request/reply caller while it needs the read half.
struct YieldTurn<'a>(&'a ReadYield);

impl ReadYield {
    fn begin(&self) -> YieldTurn<'_> {
        self.waiters.fetch_add(1, Ordering::SeqCst);
        // `notify_one` stores a permit when no reader is waiting yet, so a
        // reader that is about to wait still yields.
        self.notify.notify_one();
        YieldTurn(self)
    }

    fn requested(&self) -> bool {
        self.waiters.load(Ordering::SeqCst) > 0
    }
}

impl Drop for YieldTurn<'_> {
    fn drop(&mut self) {
        self.0.waiters.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Collects `REMOTE:` reply lines for waiting `write_json_rpc` calls, so a
/// concurrent `read_lines` cannot swallow them.
#[derive(Default)]
pub(crate) struct RpcRoute {
    waiters: AtomicUsize,
    replies: Mutex<VecDeque<String>>,
}

/// Held by a `write_json_rpc` call from before its write until it has its
/// reply; while any is held, readers route `REMOTE:` lines to [`RpcRoute`].
pub(crate) struct RpcTurn<'a>(&'a RpcRoute);

impl Drop for RpcTurn<'_> {
    fn drop(&mut self) {
        if self.0.waiters.fetch_sub(1, Ordering::SeqCst) == 1 {
            // No waiter left: a reply that arrived after its caller timed out
            // must not satisfy the next call.
            self.0
                .replies
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
    }
}

/// Borrowed view of a live session's WebSocket halves.
pub(crate) struct WsSession<'a> {
    pub(crate) rt: &'a Runtime,
    pub(crate) write: &'a Mutex<WsSink>,
    pub(crate) read: &'a Mutex<WsSource>,
    pub(crate) pending: &'a Mutex<VecDeque<String>>,
    pub(crate) read_yield: &'a ReadYield,
    /// Serializes request/reply calls with each other.
    pub(crate) requests: &'a Mutex<()>,
    pub(crate) rpc: &'a RpcRoute,
}

/// Whether a reader keeps waiting after handling one WebSocket frame.
enum Control {
    Continue,
    Stop,
}

enum Frame {
    Yield,
    Timeout,
    Message(Option<Result<tungstenite::Message, tungstenite::Error>>),
}

impl WsSession<'_> {
    /// Send one text frame. Returns false if the socket rejected it.
    pub(crate) fn send(&self, text: String) -> bool {
        let mut write = self.write.lock().unwrap_or_else(|e| e.into_inner());
        self.rt
            .block_on(write.send(tungstenite::Message::Text(text)))
            .is_ok()
    }

    fn drain_pending_into(&self, lines: &mut Vec<String>) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        lines.extend(pending.drain(..));
    }

    fn push_pending(&self, lines: Vec<String>) {
        if !lines.is_empty() {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(lines);
        }
    }

    /// Take `REMOTE:` reply lines out of `lines` for waiting RPC calls.
    fn route(&self, lines: Vec<String>) -> Vec<String> {
        if self.rpc.waiters.load(Ordering::SeqCst) == 0 {
            return lines;
        }
        let (replies, rest): (Vec<_>, Vec<_>) = lines
            .into_iter()
            .partition(|line| line.starts_with(REMOTE_PREFIX));
        if !replies.is_empty() {
            self.rpc
                .replies
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(replies);
        }
        rest
    }

    /// Handle one frame read by a serial reader; serial lines for the caller
    /// are appended to `lines`.
    fn handle_frame(
        &self,
        message: Option<Result<tungstenite::Message, tungstenite::Error>>,
        auto_reconnect: bool,
        lines: &mut Vec<String>,
    ) -> Control {
        match message {
            Some(Ok(tungstenite::Message::Text(text))) => {
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(ServerMessage::Data {
                        lines: data_lines, ..
                    }) => {
                        lines.extend(self.route(data_lines));
                        Control::Continue
                    }
                    // Paused for a deploy; keep waiting if we reattach.
                    Ok(ServerMessage::Preempted { .. }) if auto_reconnect => Control::Continue,
                    Ok(ServerMessage::Preempted { .. })
                    | Ok(ServerMessage::PortRebindFailed { .. })
                    | Ok(ServerMessage::PortDisconnected { .. }) => Control::Stop,
                    _ => Control::Continue,
                }
            }
            Some(Ok(tungstenite::Message::Close(_))) | None => Control::Stop,
            Some(_) => Control::Continue,
        }
    }

    /// Serial lines received within `timeout`: returns as soon as any are
    /// available, or empty on timeout, close, or a terminal port event.
    ///
    /// Gives the read half to request/reply callers ([`Self::request`])
    /// whenever they ask, so a long read never delays a write.
    pub(crate) fn read_lines(&self, timeout: Duration, auto_reconnect: bool) -> Vec<String> {
        let deadline = Instant::now() + timeout;
        let mut lines = Vec::new();
        loop {
            self.drain_pending_into(&mut lines);
            if !lines.is_empty() {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            if self.read_yield.requested() {
                std::thread::sleep(YIELD_POLL.min(deadline - now));
                continue;
            }
            let remaining = deadline - now;
            let frame = {
                let mut read = self.read.lock().unwrap_or_else(|e| e.into_inner());
                // The timer and the select must be created inside the
                // runtime context ("there is no reactor running" otherwise).
                self.rt.block_on(async {
                    tokio::select! {
                        biased;
                        () = self.read_yield.notify.notified() => Frame::Yield,
                        next = tokio::time::timeout(remaining, read.next()) => match next {
                            Ok(message) => Frame::Message(message),
                            Err(_) => Frame::Timeout,
                        },
                    }
                })
            };
            match frame {
                Frame::Yield => continue,
                Frame::Timeout => break,
                Frame::Message(message) => {
                    if let Control::Stop = self.handle_frame(message, auto_reconnect, &mut lines) {
                        break;
                    }
                }
            }
        }
        lines
    }

    /// Send `text`, then wait up to `wait` for the reply `on_reply` accepts.
    ///
    /// Serial lines that arrive before the reply are kept for the next
    /// [`Self::read_lines`]. Returns `None` if the send fails, the socket
    /// closes, or no reply arrives in time.
    pub(crate) fn request<T>(
        &self,
        text: String,
        wait: Duration,
        mut on_reply: impl FnMut(ServerMessage) -> Option<T>,
    ) -> Option<T> {
        // One request/reply at a time, so overlapping calls cannot consume
        // each other's replies.
        let _request = self.requests.lock().unwrap_or_else(|e| e.into_inner());
        // Ask any waiting reader for the read half before sending, so the
        // reply cannot be consumed by the reader. Declared before `read` so
        // it is released after the lock.
        let _turn = self.read_yield.begin();
        if !self.send(text) {
            return None;
        }
        let mut read = self.read.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = Instant::now() + wait;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let next = self
                .rt
                .block_on(async { tokio::time::timeout(remaining, read.next()).await });
            match next {
                Ok(Some(Ok(tungstenite::Message::Text(text)))) => {
                    match serde_json::from_str::<ServerMessage>(&text) {
                        Ok(ServerMessage::Data { lines, .. }) => {
                            let lines = self.route(lines);
                            self.push_pending(lines);
                        }
                        Ok(message) => {
                            if let Some(reply) = on_reply(message) {
                                return Some(reply);
                            }
                        }
                        Err(_) => {}
                    }
                }
                Ok(Some(Ok(tungstenite::Message::Close(_)))) | Ok(None) | Err(_) => return None,
                Ok(Some(_)) => {}
            }
        }
        None
    }

    /// Start a JSON-RPC exchange: from now until the returned turn drops,
    /// every reader hands `REMOTE:` lines to [`Self::wait_rpc_reply`]. Take
    /// the turn before writing the request.
    pub(crate) fn begin_rpc(&self) -> RpcTurn<'_> {
        self.rpc.waiters.fetch_add(1, Ordering::SeqCst);
        RpcTurn(self.rpc)
    }

    /// The JSON part of the next `REMOTE:` reply within `timeout`.
    ///
    /// Reads frames itself when the read half is free; when another thread
    /// is reading, that reader routes the reply here. Serial lines it reads
    /// that are not replies are kept for `read_lines`.
    pub(crate) fn wait_rpc_reply(&self, timeout: Duration, auto_reconnect: bool) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut stopped = false;
        loop {
            let reply = self
                .rpc
                .replies
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front();
            if let Some(reply) = reply {
                return reply.strip_prefix(REMOTE_PREFIX).map(str::to_string);
            }
            let now = Instant::now();
            if stopped || now >= deadline {
                return None;
            }
            if self.read_yield.requested() {
                std::thread::sleep(YIELD_POLL.min(deadline - now));
                continue;
            }
            let mut read = match self.read.try_lock() {
                Ok(read) => read,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(YIELD_POLL.min(deadline - now));
                    continue;
                }
            };
            let slice = (deadline - now).min(RPC_SLICE);
            let frame = self.rt.block_on(async {
                tokio::select! {
                    biased;
                    () = self.read_yield.notify.notified() => Frame::Yield,
                    next = tokio::time::timeout(slice, read.next()) => match next {
                        Ok(message) => Frame::Message(message),
                        Err(_) => Frame::Timeout,
                    },
                }
            });
            drop(read);
            if let Frame::Message(message) = frame {
                let mut lines = Vec::new();
                stopped = matches!(
                    self.handle_frame(message, auto_reconnect, &mut lines),
                    Control::Stop
                );
                self.push_pending(lines);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    /// A daemon stand-in: answers every `write` with a `write_ack` and, after
    /// `echo_delay`, echoes the written text back as a serial line. Text
    /// starting with `REMOTE:` comes back verbatim, like a device's JSON-RPC
    /// reply; anything else comes back as `echo:<text>`.
    async fn fake_daemon(listener: TcpListener, echo_delay: Duration) {
        let (stream, _) = listener.accept().await.unwrap();
        // Small back-to-back frames (ack, then echo) otherwise wait on Nagle.
        stream.set_nodelay(true).unwrap();
        let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let (sink, mut source) = ws.split();
        let sink = Arc::new(tokio::sync::Mutex::new(sink));
        while let Some(Ok(tungstenite::Message::Text(text))) = source.next().await {
            let request: serde_json::Value = serde_json::from_str(&text).unwrap();
            if request["type"] != "write" {
                continue;
            }
            let data = request["data"].as_str().unwrap().to_string();
            let ack = serde_json::json!({
                "type": "write_ack", "success": true, "bytes_written": data.len(), "message": null
            });
            sink.lock()
                .await
                .send(tungstenite::Message::Text(ack.to_string()))
                .await
                .unwrap();
            let echoed = if data.starts_with(REMOTE_PREFIX) {
                data.clone()
            } else {
                format!("echo:{data}")
            };
            let line = serde_json::json!({
                "type": "data", "lines": [echoed], "current_index": 0
            })
            .to_string();
            if echo_delay.is_zero() {
                // Inline, so echoes keep the order of the writes.
                sink.lock()
                    .await
                    .send(tungstenite::Message::Text(line))
                    .await
                    .unwrap();
                continue;
            }
            let sink = Arc::clone(&sink);
            tokio::spawn(async move {
                tokio::time::sleep(echo_delay).await;
                let _ = sink
                    .lock()
                    .await
                    .send(tungstenite::Message::Text(line))
                    .await;
            });
        }
    }

    struct Session {
        rt: &'static Runtime,
        write: Mutex<WsSink>,
        read: Mutex<WsSource>,
        pending: Mutex<VecDeque<String>>,
        read_yield: ReadYield,
        requests: Mutex<()>,
        rpc: RpcRoute,
    }

    impl Session {
        fn view(&self) -> WsSession<'_> {
            WsSession {
                rt: self.rt,
                write: &self.write,
                read: &self.read,
                pending: &self.pending,
                read_yield: &self.read_yield,
                requests: &self.requests,
                rpc: &self.rpc,
            }
        }
    }

    fn connect(echo_delay: Duration) -> Arc<Session> {
        let rt: &'static Runtime = Box::leak(Box::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
        ));
        let (write, read) = rt.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            tokio::spawn(fake_daemon(listener, echo_delay));
            let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            ws.split()
        });
        Arc::new(Session {
            rt,
            write: Mutex::new(write),
            read: Mutex::new(read),
            pending: Mutex::default(),
            read_yield: ReadYield::default(),
            requests: Mutex::default(),
            rpc: RpcRoute::default(),
        })
    }

    fn write(session: &WsSession<'_>, data: &str) -> Option<usize> {
        let msg = serde_json::json!({ "type": "write", "data": data }).to_string();
        session.request(msg, Duration::from_secs(5), |reply| match reply {
            ServerMessage::WriteAck { bytes_written, .. } => Some(bytes_written),
            _ => None,
        })
    }

    /// FastLED/fbuild#1431: a write issued while another thread is blocked
    /// in a long `read_lines` must not wait out that read's timeout, and the
    /// reply line must still reach the reader.
    #[test]
    fn write_does_not_wait_for_an_in_flight_read() {
        let session = connect(Duration::from_millis(20));
        let reader = {
            let session = Arc::clone(&session);
            std::thread::spawn(move || {
                let started = Instant::now();
                let lines = session.view().read_lines(Duration::from_secs(10), true);
                (lines, started.elapsed())
            })
        };
        // Let the reader block on the socket first.
        std::thread::sleep(Duration::from_millis(100));

        let started = Instant::now();
        assert_eq!(write(&session.view(), "ping"), Some(4));
        let write_time = started.elapsed();
        assert!(
            write_time < Duration::from_secs(2),
            "write waited {write_time:?} behind the in-flight read"
        );

        let (lines, read_time) = reader.join().unwrap();
        assert_eq!(lines, ["echo:ping"]);
        assert!(
            read_time < Duration::from_secs(5),
            "the reader should return with the reply, not at its 10s timeout ({read_time:?})"
        );
    }

    /// A reply line that arrives while the writer still holds the read half
    /// (before its `write_ack` is consumed) is kept for the next read.
    #[test]
    fn lines_seen_during_a_request_are_kept_for_the_next_read() {
        let session = connect(Duration::ZERO);
        let view = session.view();
        assert_eq!(write(&view, "a"), Some(1));
        assert_eq!(write(&view, "b"), Some(1));
        let mut lines = Vec::new();
        while lines.len() < 2 {
            let batch = view.read_lines(Duration::from_secs(2), true);
            assert!(!batch.is_empty(), "echoes were lost: {lines:?}");
            lines.extend(batch);
        }
        assert_eq!(lines, ["echo:a", "echo:b"]);
    }

    #[test]
    fn read_lines_times_out_empty() {
        let session = connect(Duration::ZERO);
        let started = Instant::now();
        let lines = session.view().read_lines(Duration::from_millis(150), true);
        assert!(lines.is_empty());
        assert!(started.elapsed() >= Duration::from_millis(150));
    }

    /// A `write_json_rpc` reply must reach the RPC waiter even while another
    /// thread sits in `read_lines`, and even when it arrives several empty
    /// read slices later; the reader must not see it.
    #[test]
    fn rpc_reply_reaches_the_waiter_despite_a_concurrent_reader() {
        let session = connect(RPC_SLICE * 3);
        let reader = {
            let session = Arc::clone(&session);
            std::thread::spawn(move || session.view().read_lines(Duration::from_secs(1), true))
        };
        std::thread::sleep(Duration::from_millis(100));

        let view = session.view();
        let turn = view.begin_rpc();
        assert_eq!(write(&view, "REMOTE:{\"id\":1}"), Some(15));
        let reply = view.wait_rpc_reply(Duration::from_secs(5), true);
        drop(turn);
        assert_eq!(reply.as_deref(), Some("{\"id\":1}"));
        assert!(
            reader.join().unwrap().is_empty(),
            "the concurrent reader must not receive the RPC reply"
        );
    }

    /// Overlapping request/reply calls from several threads each get their
    /// own reply, never another call's.
    #[test]
    fn overlapping_writes_each_get_their_own_ack() {
        let session = connect(Duration::ZERO);
        for _round in 0..5 {
            let start = Arc::new(std::sync::Barrier::new(32));
            let writers: Vec<_> = (1..=32)
                .map(|len| {
                    let (session, start) = (Arc::clone(&session), Arc::clone(&start));
                    std::thread::spawn(move || {
                        start.wait();
                        (len, write(&session.view(), &"x".repeat(len)))
                    })
                })
                .collect();
            for writer in writers {
                let (len, acked) = writer.join().unwrap();
                assert_eq!(acked, Some(len), "a write got another write's ack");
            }
        }
    }
}

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

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::runtime::Runtime;
use tokio_tungstenite::tungstenite;

use crate::messages::{ServerMessage, WsSink, WsSource};

/// How often a reader that yielded re-checks whether the read half is free
/// again. Request/reply callers hold it only until their reply arrives.
const YIELD_POLL: Duration = Duration::from_millis(1);

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

/// Borrowed view of a live session's WebSocket halves.
pub(crate) struct WsSession<'a> {
    pub(crate) rt: &'a Runtime,
    pub(crate) write: &'a Mutex<WsSink>,
    pub(crate) read: &'a Mutex<WsSource>,
    pub(crate) pending: &'a Mutex<VecDeque<String>>,
    pub(crate) read_yield: &'a ReadYield,
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
                Frame::Message(Some(Ok(tungstenite::Message::Text(text)))) => {
                    match serde_json::from_str::<ServerMessage>(&text) {
                        Ok(ServerMessage::Data {
                            lines: data_lines, ..
                        }) => lines.extend(data_lines),
                        // Paused for a deploy; keep waiting if we reattach.
                        Ok(ServerMessage::Preempted { .. }) if auto_reconnect => continue,
                        Ok(ServerMessage::Preempted { .. })
                        | Ok(ServerMessage::PortRebindFailed { .. })
                        | Ok(ServerMessage::PortDisconnected { .. }) => break,
                        _ => continue,
                    }
                }
                Frame::Message(Some(Ok(tungstenite::Message::Close(_)))) | Frame::Message(None) => {
                    break;
                }
                Frame::Message(_) => continue,
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
                        Ok(ServerMessage::Data { lines, .. }) => self.push_pending(lines),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    /// A daemon stand-in: answers every `write` with a `write_ack` and, after
    /// `echo_delay`, echoes the written text back as a serial line.
    async fn fake_daemon(listener: TcpListener, echo_delay: Duration) {
        let (stream, _) = listener.accept().await.unwrap();
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
            let line = serde_json::json!({
                "type": "data", "lines": [format!("echo:{data}")], "current_index": 0
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
    }

    impl Session {
        fn view(&self) -> WsSession<'_> {
            WsSession {
                rt: self.rt,
                write: &self.write,
                read: &self.read,
                pending: &self.pending,
                read_yield: &self.read_yield,
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
}

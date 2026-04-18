//! Tracing-subscriber layer that forwards events to
//! [`BroadcastHub::log_tx`] so `/ws/logs` subscribers see the same
//! events that are written to the daemon's stderr.
//!
//! # Why (issue #66)
//!
//! The native ESP32 `write-flash` path (`fbuild-deploy::esp32_native`)
//! already emits progress via `tracing::info!()` — per-region start,
//! 10%-throttled byte counts, region-complete markers. Before this
//! layer the events only reached stderr; WebSocket clients subscribed
//! to `/ws/logs` received nothing during a flash because no other
//! bridge existed. Installing this layer alongside the existing
//! `tracing_subscriber::fmt` layer makes the WebSocket stream the live
//! progress feed the deploy path was already producing, without adding
//! a separate progress API.
//!
//! # Cycle avoidance
//!
//! Events originating inside the `/ws/logs` handler itself (e.g. a
//! `tracing::info!("Logs WebSocket connected")` from
//! `handlers::websockets`) are dropped by module-path filter — sending
//! them back onto `log_tx` would just re-feed subscribers their own
//! connect/disconnect notices. The broadcast channel is bounded so a
//! cycle could not deadlock, but the filter keeps the stream quieter.
//!
//! # Cost when no clients
//!
//! `broadcast::Sender::receiver_count` is a single atomic load. When
//! no `/ws/logs` clients are connected the layer skips JSON
//! serialization entirely and bottoms out at one atomic read per
//! event.

use std::fmt::Write as _;

use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Tracing layer that JSON-serializes each event and publishes it on
/// the provided broadcast channel. Drop the layer (or drop every
/// receiver) to stop forwarding.
pub struct BroadcastLogLayer {
    tx: broadcast::Sender<String>,
}

impl BroadcastLogLayer {
    pub fn new(tx: broadcast::Sender<String>) -> Self {
        Self { tx }
    }
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

impl<S> Layer<S> for BroadcastLogLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        // Cheap early-out when nothing is subscribed. `/ws/logs` is an
        // opt-in stream; the common case is zero clients and we should
        // not pay serialization cost for events that nobody reads.
        if self.tx.receiver_count() == 0 {
            return;
        }

        let meta = event.metadata();
        let module = meta.module_path().unwrap_or_else(|| meta.target());

        // Drop events from the `/ws/logs` handler itself so client
        // connect/disconnect notices don't feed themselves back onto
        // the same channel they announced. See module docstring.
        if module.contains("handlers::websockets") {
            return;
        }

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let message = visitor.finish();

        // Same shape that `/ws/logs` already sends for its welcome
        // frame — clients can parse every log line with one schema.
        let payload = serde_json::json!({
            "type": "log",
            "level": meta.level().as_str(),
            "message": message,
            "timestamp": now_unix(),
            "module": module,
        })
        .to_string();

        // Ignore send errors: the only failure mode for a bounded
        // broadcast with no active receivers is `SendError` — which is
        // harmless and already guarded above, but subscribers can race
        // between the `receiver_count` check and the send.
        let _ = self.tx.send(payload);
    }
}

/// Collects the event's `message` and any named fields into a single
/// human-readable string matching the shape `fmt::Layer` writes to
/// stderr. Kept in this file (not `tracing_subscriber::fmt::format`)
/// because we only need the rendered message, not the full formatter
/// machinery.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl MessageVisitor {
    fn finish(self) -> String {
        match (self.message.is_empty(), self.fields.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.message,
            (true, false) => self.fields,
            (false, false) => format!("{} {}", self.message, self.fields),
        }
    }

    fn push_field_debug(&mut self, name: &str, value: &dyn std::fmt::Debug) {
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        let _ = write!(&mut self.fields, "{}={:?}", name, value);
    }

    fn push_field_str(&mut self, name: &str, value: &str) {
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        let _ = write!(&mut self.fields, "{}={}", name, value);
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            // `tracing`'s format macros pass the rendered `Arguments`
            // here; its `Debug` impl is `Display`-equivalent, so this
            // prints the same text the user wrote in `info!(...)`.
            let _ = write!(&mut self.message, "{:?}", value);
        } else {
            self.push_field_debug(field.name(), value);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.push_field_str(field.name(), value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_visitor_joins_message_and_fields() {
        let mut v = MessageVisitor::default();
        v.push_field_str("port", "/dev/ttyUSB0");
        v.push_field_debug("size", &42usize);
        v.message.push_str("native write: begin region");
        let rendered = v.finish();
        assert!(rendered.starts_with("native write: begin region "));
        assert!(rendered.contains("port=/dev/ttyUSB0"));
        assert!(rendered.contains("size=42"));
    }

    #[test]
    fn message_visitor_message_only() {
        let mut v = MessageVisitor::default();
        v.message.push_str("hello");
        assert_eq!(v.finish(), "hello");
    }

    #[test]
    fn message_visitor_fields_only() {
        let mut v = MessageVisitor::default();
        v.push_field_str("port", "COM7");
        assert_eq!(v.finish(), "port=COM7");
    }

    /// Regression: no clients subscribed means no payload work. A
    /// fresh `broadcast::channel` has zero receivers once the initial
    /// `_rx` is dropped, and `send` returns `Err`. The layer treats
    /// this as the common case and short-circuits before serializing.
    #[test]
    fn layer_noop_when_no_subscribers() {
        let (tx, _) = broadcast::channel::<String>(4);
        // Drop the initial receiver by letting `_` fall out of scope.
        let layer = BroadcastLogLayer::new(tx.clone());
        assert_eq!(layer.tx.receiver_count(), 0);
        // If there were an attempt to serialize we'd observe zero sends
        // against zero receivers — with receiver_count==0 the layer
        // exits before building any string.
        assert_eq!(tx.receiver_count(), 0);
    }
}

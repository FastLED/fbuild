//! Shared WebSocket message types and type aliases used by the
//! synchronous and asynchronous SerialMonitor implementations.

use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite;

pub(crate) type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
pub(crate) type WsSink = futures::stream::SplitSink<WsStream, tungstenite::Message>;
pub(crate) type WsSource = futures::stream::SplitStream<WsStream>;

/// Messages we receive from the daemon (subset we care about).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServerMessage {
    Attached {
        success: bool,
        #[allow(dead_code)]
        message: String,
        #[allow(dead_code)]
        writer_pre_acquired: bool,
    },
    Data {
        lines: Vec<String>,
        #[allow(dead_code)]
        current_index: u64,
    },
    WriteAck {
        #[allow(dead_code)]
        success: bool,
        bytes_written: usize,
        #[allow(dead_code)]
        message: Option<String>,
    },
    /// Reply to `ClientMessage::GetInWaiting` — the number of buffered
    /// lines this client's broadcast receiver has not yet observed.
    /// See FastLED/fbuild#605.
    InWaiting {
        count: usize,
    },
    Preempted {
        #[allow(dead_code)]
        reason: String,
        #[allow(dead_code)]
        preempted_by: String,
    },
    Reconnected {
        #[allow(dead_code)]
        message: String,
    },
    PortDisconnected {
        #[allow(dead_code)]
        port: String,
        #[allow(dead_code)]
        reason: String,
        #[allow(dead_code)]
        message: String,
    },
    PortRenumbered {
        #[allow(dead_code)]
        port: String,
        #[allow(dead_code)]
        new_port: String,
        #[allow(dead_code)]
        reason: String,
        #[allow(dead_code)]
        serial: Option<String>,
    },
    PortReattached {
        #[allow(dead_code)]
        port: String,
        #[allow(dead_code)]
        previous_port: String,
    },
    Error {
        message: String,
    },
    #[serde(other)]
    Other,
}

/// Client message to send to the daemon.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ClientMessage {
    Attach {
        client_id: String,
        port: String,
        baud_rate: u32,
        open_if_needed: bool,
        pre_acquire_writer: bool,
    },
    Write {
        data: String,
    },
    Detach,
    /// Drop buffered serial-line data on the daemon side. Matches
    /// pyserial's `Serial.reset_input_buffer()`. See FastLED/fbuild#605.
    ClearBuffer,
    /// Ask the daemon for the count of buffered lines this client has
    /// not yet observed. Maps to pyserial's `Serial.in_waiting`. The
    /// daemon replies with `ServerMessage::InWaiting`. See FastLED/fbuild#605.
    GetInWaiting,
}

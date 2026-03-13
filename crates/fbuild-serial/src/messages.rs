//! WebSocket message types for the serial monitor API.
//!
//! These match the Python implementation's message protocol exactly:
//! - Client → Server: attach, write, detach
//! - Server → Client: attached, data, preempted, reconnected, write_ack, error

use serde::{Deserialize, Serialize};

/// Messages sent by the client to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SerialClientMessage {
    Attach {
        client_id: String,
        port: String,
        baud_rate: u32,
        open_if_needed: bool,
        pre_acquire_writer: bool,
    },
    Write {
        /// Base64-encoded data.
        data: String,
    },
    Detach,
}

/// Messages sent by the daemon to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SerialServerMessage {
    Attached {
        success: bool,
        message: String,
        writer_pre_acquired: bool,
    },
    Data {
        lines: Vec<String>,
        current_index: u64,
    },
    Preempted {
        reason: String,
        preempted_by: String,
    },
    Reconnected {
        message: String,
    },
    WriteAck {
        success: bool,
        bytes_written: usize,
    },
    Error {
        message: String,
    },
}

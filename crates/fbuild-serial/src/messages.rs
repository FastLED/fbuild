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
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SerialClientMessage ---

    #[test]
    fn client_attach_roundtrip() {
        let msg = SerialClientMessage::Attach {
            client_id: "c1".into(),
            port: "COM3".into(),
            baud_rate: 115200,
            open_if_needed: true,
            pre_acquire_writer: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"attach\""));
        let parsed: SerialClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            SerialClientMessage::Attach {
                client_id,
                port,
                baud_rate,
                open_if_needed,
                pre_acquire_writer,
            } => {
                assert_eq!(client_id, "c1");
                assert_eq!(port, "COM3");
                assert_eq!(baud_rate, 115200);
                assert!(open_if_needed);
                assert!(!pre_acquire_writer);
            }
            _ => panic!("expected Attach"),
        }
    }

    #[test]
    fn client_write_roundtrip() {
        let msg = SerialClientMessage::Write {
            data: "aGVsbG8=".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"write\""));
        let parsed: SerialClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            SerialClientMessage::Write { data } => assert_eq!(data, "aGVsbG8="),
            _ => panic!("expected Write"),
        }
    }

    #[test]
    fn client_detach_roundtrip() {
        let msg = SerialClientMessage::Detach;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"detach\""));
        let parsed: SerialClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, SerialClientMessage::Detach));
    }

    // --- SerialServerMessage ---

    #[test]
    fn server_attached_roundtrip() {
        let msg = SerialServerMessage::Attached {
            success: true,
            message: "ok".into(),
            writer_pre_acquired: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"attached\""));
        let parsed: SerialServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            SerialServerMessage::Attached {
                success,
                writer_pre_acquired,
                ..
            } => {
                assert!(success);
                assert!(writer_pre_acquired);
            }
            _ => panic!("expected Attached"),
        }
    }

    #[test]
    fn server_data_roundtrip() {
        let msg = SerialServerMessage::Data {
            lines: vec!["hello".into(), "world".into()],
            current_index: 42,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"data\""));
        let parsed: SerialServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            SerialServerMessage::Data {
                lines,
                current_index,
            } => {
                assert_eq!(lines, vec!["hello", "world"]);
                assert_eq!(current_index, 42);
            }
            _ => panic!("expected Data"),
        }
    }

    #[test]
    fn server_preempted_roundtrip() {
        let msg = SerialServerMessage::Preempted {
            reason: "deploy".into(),
            preempted_by: "req-1".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"preempted\""));
        let parsed: SerialServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            SerialServerMessage::Preempted {
                reason,
                preempted_by,
            } => {
                assert_eq!(reason, "deploy");
                assert_eq!(preempted_by, "req-1");
            }
            _ => panic!("expected Preempted"),
        }
    }

    #[test]
    fn server_reconnected_roundtrip() {
        let msg = SerialServerMessage::Reconnected {
            message: "back".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"reconnected\""));
    }

    #[test]
    fn server_write_ack_without_message() {
        let msg = SerialServerMessage::WriteAck {
            success: true,
            bytes_written: 5,
            message: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"write_ack\""));
        assert!(!json.contains("\"message\""));
    }

    #[test]
    fn server_write_ack_with_message() {
        let msg = SerialServerMessage::WriteAck {
            success: false,
            bytes_written: 0,
            message: Some("write error: timeout".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"message\""));
        assert!(json.contains("write error: timeout"));
    }

    #[test]
    fn server_write_ack_roundtrip_with_optional_message() {
        // Ensure message: null or absent still deserializes to None
        let json = r#"{"type":"write_ack","success":true,"bytes_written":3}"#;
        let parsed: SerialServerMessage = serde_json::from_str(json).unwrap();
        match parsed {
            SerialServerMessage::WriteAck {
                success,
                bytes_written,
                message,
            } => {
                assert!(success);
                assert_eq!(bytes_written, 3);
                assert!(message.is_none());
            }
            _ => panic!("expected WriteAck"),
        }
    }

    #[test]
    fn server_error_roundtrip() {
        let msg = SerialServerMessage::Error {
            message: "bad".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"error\""));
        let parsed: SerialServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            SerialServerMessage::Error { message } => assert_eq!(message, "bad"),
            _ => panic!("expected Error"),
        }
    }
}

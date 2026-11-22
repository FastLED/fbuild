//! Regression tests for FastLED/fbuild#2151: `fbuild reset` used to call
//! `fbuild_deploy::reset::reset_device` in-process, bypassing the daemon's
//! device-lease admission, so it could reset a board another workflow had
//! leased and was validating.
//!
//! These tests pin the wire contract of the fixed path — the CLI must send the
//! reset to `POST /api/reset` carrying the caller's lease id, and must surface
//! an admission refusal rather than a reset result — against a stub daemon, so
//! they run without hardware or a real daemon.

use super::reset_via;
use crate::daemon_client::DaemonClient;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

/// One canned HTTP response plus the request the stub actually received.
struct StubDaemon {
    port: u16,
    received: mpsc::Receiver<String>,
}

impl StubDaemon {
    /// Bind an ephemeral port and answer exactly one request with `status` and
    /// `body`. Returns the port plus a channel that yields the raw request
    /// once it has been served.
    fn start(status_line: &str, body: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        let (tx, received) = mpsc::channel();
        let status_line = status_line.to_string();
        let body = body.to_string();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept one connection");
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut head = String::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if let Some(value) = line
                    .to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().to_string())
                {
                    content_length = value.parse().unwrap_or(0);
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body_bytes = vec![0u8; content_length];
            if content_length > 0 {
                reader
                    .read_exact(&mut body_bytes)
                    .expect("read request body");
            }
            let request = format!("{head}\n{}", String::from_utf8_lossy(&body_bytes));
            let response = format!(
                "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let mut stream = stream;
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            stream.flush().ok();
            let _ = tx.send(request);
        });
        Self { port, received }
    }

    fn request(self) -> String {
        self.received
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("stub daemon received the request")
    }
}

fn json_field(request: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = request.find(&needle)? + needle.len();
    let rest = &request[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The reset the CLI emits must name the port, the board (so the daemon picks
/// the same platform-specific sequence) and the caller's own lease id.
#[tokio::test]
async fn reset_posts_port_board_and_lease_id_to_the_daemon() {
    let stub = StubDaemon::start(
        "HTTP/1.1 200 OK",
        r#"{"success":true,"request_id":"stub","message":"device reset successful on /dev/ttyACM1","exit_code":0,"output_file":null,"output_dir":null,"launch_url":null}"#,
    );
    let client = DaemonClient::with_port(stub.port);

    reset_via(
        &client,
        "/dev/ttyACM1".to_string(),
        "esp32dev",
        Some("lease-abc".to_string()),
        false,
    )
    .await
    .expect("reset admitted on the daemon");

    let request = stub.request();
    assert!(
        request.starts_with("POST /api/reset"),
        "reset must go through the daemon route, got: {request}"
    );
    assert_eq!(
        json_field(&request, "port").as_deref(),
        Some("/dev/ttyACM1")
    );
    assert_eq!(json_field(&request, "board").as_deref(), Some("esp32dev"));
    assert_eq!(
        json_field(&request, "lease_id").as_deref(),
        Some("lease-abc"),
        "the caller's lease id must be presented for admission"
    );
}

/// A foreign exclusive lease is an admission refusal: the CLI must report the
/// daemon's explanation and must not claim the reset ran.
#[tokio::test]
async fn foreign_lease_refusal_surfaces_the_daemon_message() {
    let holder = "device '/dev/ttyACM1' is leased by client 'other-flow' (lease 7c1d, exclusive). \
                  pass --lease-id 7c1d to operate under that lease";
    let body = serde_json::json!({
        "success": false,
        "request_id": "stub",
        "message": holder,
        "exit_code": 1,
        "output_file": null,
        "output_dir": null,
        "launch_url": null,
    })
    .to_string();
    let stub = StubDaemon::start("HTTP/1.1 409 Conflict", &body);
    let client = DaemonClient::with_port(stub.port);

    let error = reset_via(&client, "/dev/ttyACM1".to_string(), "esp32dev", None, false)
        .await
        .expect_err("a leased board must be refused");

    let message = error.to_string();
    assert!(
        message.contains("leased by client 'other-flow'"),
        "refusal must keep the daemon's explanation, got: {message}"
    );
    assert!(
        message.contains("--lease-id"),
        "refusal must tell the caller how to proceed, got: {message}"
    );
    let request = stub.request();
    assert!(
        request.contains("/api/reset"),
        "the CLI must ask the daemon instead of opening the port itself"
    );
}

/// An unleased board resets normally: no lease in the body, success reported.
#[tokio::test]
async fn unleased_board_resets_without_a_lease_id() {
    let stub = StubDaemon::start(
        "HTTP/1.1 200 OK",
        r#"{"success":true,"request_id":"stub","message":"device reset successful on COM4","exit_code":0,"output_file":null,"output_dir":null,"launch_url":null}"#,
    );
    let client = DaemonClient::with_port(stub.port);

    reset_via(&client, "COM4".to_string(), "uno", None, false)
        .await
        .expect("an unleased board is admitted");

    let request = stub.request();
    assert!(
        json_field(&request, "lease_id").is_none(),
        "an absent lease must not be sent as a field, got: {request}"
    );
}

//! The attach handshake of a serial session: the `Attach` frame
//! (FastLED/fbuild#1864) and the bounded connect/attach/reply exchange
//! (FastLED/fbuild#2095).
//!
//! Lives in a sibling module so `serial_session.rs` stays under the LOC gate.
//! Keeping the config-to-frame mapping in one function is what makes the
//! workflow lease testable without a daemon: the daemon admits the attach on
//! `lease_id`, so a Python workflow that took a lease must present it here or
//! its own session is refused as a foreign lease.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite;

use crate::messages::{ClientMessage, ServerMessage, WsSink, WsSource};

use super::{SessionConfig, SessionError};

/// Headroom over the daemon's own attach bounds for scheduling delay on a
/// loaded host and the WebSocket round trips themselves.
const ATTACH_HANDSHAKE_MARGIN: Duration = Duration::from_secs(4);

/// How long a facade waits for the daemon to attach (FastLED/fbuild#2095).
///
/// The daemon may legitimately spend its pre-open family-resolution bound and
/// then its open cap before it answers. A shorter client budget gave up on
/// attaches the daemon went on to complete (96% of attaches on a starved
/// host with the old flat 5 s), so the budget is derived from those bounds.
pub(crate) const ATTACH_HANDSHAKE_TIMEOUT: Duration = fbuild_serial::OPEN_FAMILY_RESOLVE_TIMEOUT
    .saturating_add(fbuild_serial::WS_SERIAL_OPEN_PORT_TIMEOUT)
    .saturating_add(ATTACH_HANDSHAKE_MARGIN);

/// How far an attach got before its budget ran out. Lets a caller tell a
/// daemon that never answered from one that is alive but starved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandshakeStage {
    Connecting,
    SendingAttach,
    AwaitingAttachReply,
}

impl HandshakeStage {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            HandshakeStage::Connecting => "connecting to the daemon WebSocket (no connection)",
            HandshakeStage::SendingAttach => "sending the attach request (connected)",
            HandshakeStage::AwaitingAttachReply => {
                "waiting for the daemon's attach reply (connected and attach sent; \
                 the daemon is alive but has not finished opening the port)"
            }
        }
    }
}

/// Build the `Attach` frame a session sends for `cfg`.
pub(crate) fn attach_message(cfg: &SessionConfig) -> ClientMessage {
    ClientMessage::Attach {
        client_id: cfg.client_id.clone(),
        port: cfg.port.clone(),
        baud_rate: cfg.baud_rate,
        open_if_needed: true,
        pre_acquire_writer: true,
        client_metadata: Some(crate::messages::ClientMetadata::current()),
        lease_id: cfg.lease_id.clone(),
    }
}

/// Connect, send the `Attach` frame and read the daemon's verdict, all
/// within `cfg.handshake_timeout`.
pub(crate) async fn handshake(cfg: &SessionConfig) -> Result<(WsSink, WsSource), SessionError> {
    let deadline = tokio::time::Instant::now() + cfg.handshake_timeout;
    let timed_out = |stage| SessionError::HandshakeTimeout {
        stage,
        waited: cfg.handshake_timeout,
    };
    let (ws_stream, _) = match tokio::time::timeout_at(
        deadline,
        tokio_tungstenite::connect_async(&cfg.ws_url),
    )
    .await
    {
        Ok(Ok(ok)) => ok,
        Ok(Err(error)) => {
            return Err(SessionError::ConnectionFailed(format!(
                "failed to connect to daemon WebSocket at {}: {error}",
                cfg.ws_url
            )));
        }
        Err(_) => return Err(timed_out(HandshakeStage::Connecting)),
    };
    let (mut write, mut read) = ws_stream.split();

    let attach_json = serde_json::to_string(&attach_message(cfg))
        .expect("fbuild-python: ClientMessage::Attach serialization is infallible");
    match tokio::time::timeout_at(
        deadline,
        write.send(tungstenite::Message::Text(attach_json.into())),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return Err(SessionError::Closed),
        Err(_) => return Err(timed_out(HandshakeStage::SendingAttach)),
    }

    let msg = match tokio::time::timeout_at(deadline, read.next()).await {
        Ok(Some(Ok(msg))) => msg,
        Ok(Some(Err(_))) | Ok(None) => return Err(SessionError::Closed),
        Err(_) => return Err(timed_out(HandshakeStage::AwaitingAttachReply)),
    };
    let tungstenite::Message::Text(text) = msg else {
        return Err(SessionError::Closed);
    };
    match serde_json::from_str::<ServerMessage>(&text) {
        Ok(ServerMessage::Attached { success: true, .. }) => {
            if cfg.verbose {
                eprintln!("attached");
            }
            Ok((write, read))
        }
        // FastLED/fbuild#2083: an `error` frame (open_port failure or
        // timeout) carries the reason a bare `session closed` used to hide.
        Ok(ServerMessage::Error { message } | ServerMessage::Attached { message, .. }) => Err(
            SessionError::ConnectionFailed(format!("daemon rejected serial attach: {message}")),
        ),
        _ => Err(SessionError::Closed),
    }
}

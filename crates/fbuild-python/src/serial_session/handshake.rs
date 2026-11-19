//! Client-side serial-attach budget and timeout diagnostics
//! (FastLED/fbuild#2095).
//!
//! The daemon's attach path is bounded but not instant: it resolves the board
//! family under `fbuild_serial::OPEN_FAMILY_RESOLVE_TIMEOUT` (3 s) and *then*
//! opens the port under the daemon's 3 s `WS_SERIAL_OPEN_PORT_TIMEOUT`. A 5 s
//! client budget therefore expired while a healthy but CPU-starved daemon was
//! still working: on a 16-core host with 100 `yes` hogs (load 106-148) 54 of
//! 56 attaches timed out, and every failure in the report was on a host with a
//! sustained run queue.
//!
//! [`DEFAULT_HANDSHAKE_TIMEOUT`] covers that 6 s worst case with margin, and an
//! expired budget names the stage the client had reached, so a caller can tell
//! "daemon not accepting" from "daemon starved while opening the port".

use std::time::Duration;
use tokio::time::Instant;

use super::SessionError;

/// Client budget for one serial attach: WebSocket upgrade, attach frame and
/// the daemon's attach reply.
///
/// Must stay above the daemon's own worst case — 3 s family resolution
/// (`fbuild_serial::OPEN_FAMILY_RESOLVE_TIMEOUT`) plus the 3 s port-open cap
/// (`WS_SERIAL_OPEN_PORT_TIMEOUT` in `fbuild-daemon`) — with margin for a
/// starved event loop. FastLED/fbuild#2095.
pub(crate) const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How far an attach got before its budget expired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandshakeStage {
    /// No WebSocket upgrade completed: the daemon is not accepting.
    Connecting,
    /// Upgrade completed, but the attach frame did not reach the socket.
    SendingAttach,
    /// Attach frame sent; the daemon sent no reply within the budget.
    AwaitingReply,
}

impl HandshakeStage {
    /// Outcome text for the timeout error, phrased so starved and dead differ.
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Connecting => "no WebSocket upgrade completed (daemon not accepting)",
            Self::SendingAttach => "WebSocket connected, but the attach frame was not sent",
            Self::AwaitingReply => {
                "WebSocket connected and attach sent, but the daemon sent no reply"
            }
        }
    }
}

/// The start instant plus the single deadline the attach phases share.
pub(crate) struct HandshakeClock {
    started: Instant,
    deadline: Instant,
}

impl HandshakeClock {
    pub(crate) fn new(budget: Duration) -> Self {
        let started = Instant::now();
        Self {
            started,
            deadline: started + budget,
        }
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Error for an expired budget, naming the stage that was waiting.
    pub(crate) fn expired(&self, stage: HandshakeStage) -> SessionError {
        SessionError::ConnectionFailed(format!(
            "daemon WebSocket attach/handshake timed out after {:.1}s: {}; the daemon's attach \
             path is bounded at 3s family resolution + 3s port open, so a starved daemon can \
             outlast a short client budget (FastLED/fbuild#2095)",
            self.started.elapsed().as_secs_f32(),
            stage.describe()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_covers_the_daemons_worst_case() {
        // The daemon resolves the family (3 s) and then opens the port under
        // its own 3 s WS_SERIAL_OPEN_PORT_TIMEOUT; a client budget at or below
        // that sum gives up on a daemon that is still working (#2095).
        let worst_case = fbuild_serial::OPEN_FAMILY_RESOLVE_TIMEOUT + Duration::from_secs(3);
        assert!(
            DEFAULT_HANDSHAKE_TIMEOUT > worst_case,
            "client budget {DEFAULT_HANDSHAKE_TIMEOUT:?} must exceed the daemon's {worst_case:?} attach worst case"
        );
    }

    #[test]
    fn expired_error_names_the_stage_and_the_wait() {
        let clock = HandshakeClock::new(Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(15));
        let message = clock.expired(HandshakeStage::AwaitingReply).to_string();
        assert!(
            message.contains("attach/handshake timed out after"),
            "{message}"
        );
        assert!(
            message.contains("attach sent, but the daemon sent no reply"),
            "{message}"
        );
        assert!(message.contains("2095"), "{message}");
    }

    #[test]
    fn every_stage_describes_a_distinct_outcome() {
        let outcomes = [
            HandshakeStage::Connecting.describe(),
            HandshakeStage::SendingAttach.describe(),
            HandshakeStage::AwaitingReply.describe(),
        ];
        assert_eq!(outcomes.len(), 3);
        assert_ne!(outcomes[0], outcomes[1]);
        assert_ne!(outcomes[1], outcomes[2]);
        assert_ne!(outcomes[0], outcomes[2]);
    }
}

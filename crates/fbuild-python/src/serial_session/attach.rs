//! `Attach` frame construction for a serial session (FastLED/fbuild#1864).
//!
//! Lives in a sibling module so `serial_session.rs` stays under the LOC gate.
//! Keeping the config-to-frame mapping in one function is what makes the
//! workflow lease testable without a daemon: the daemon admits the attach on
//! `lease_id`, so a Python workflow that took a lease must present it here or
//! its own session is refused as a foreign lease.

use crate::messages::ClientMessage;

use super::SessionConfig;

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

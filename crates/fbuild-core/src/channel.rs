//! Channel bridge — wraps `tokio::sync::mpsc`.
//!
//! FastLED/fbuild#844 (bridge sweep). All async channels in the
//! workspace flow through this module so the workspace has one source
//! of truth for the channel surface. The two matching dylints are:
//!
//! - `ban_std_mpsc_in_async_reachable` — `std::sync::mpsc::*` doesn't
//!   integrate with the tokio reactor; blocking `recv()` from inside
//!   an `async fn` starves the worker.
//! - `ban_tokio_mpsc_direct_import` — direct `tokio::sync::mpsc`
//!   imports bypass this curated surface.
//!
//! The renames (`channel` → `bounded`, `unbounded_channel` →
//! `unbounded`) match the conventions zccache and a handful of other
//! Rust services use — the upstream names are awkward when both are in
//! scope.

pub use tokio::sync::mpsc::{
    channel as bounded, unbounded_channel as unbounded, Receiver, Sender, UnboundedReceiver,
    UnboundedSender,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_round_trip() {
        let (tx, mut rx) = bounded::<u32>(4);
        tx.send(7).await.unwrap();
        assert_eq!(rx.recv().await, Some(7));
    }

    #[tokio::test]
    async fn unbounded_round_trip() {
        let (tx, mut rx) = unbounded::<u32>();
        tx.send(42).unwrap();
        assert_eq!(rx.recv().await, Some(42));
    }
}

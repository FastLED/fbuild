//! Neutral pseudo-terminal support for tests that need a real serial device.
//!
//! Handler tests such as the daemon's WebSocket serial monitor drive the
//! production code against a terminal that behaves like an attached board:
//! they hand fbuild the slave device name and observe what the writer emitted
//! by reading the master side. Allocating that pair is a host mechanic, so the
//! native call lives in the concrete per-OS tree and callers only see this
//! opaque pair.
//!
//! A host that provides no openable pseudo-terminal reports
//! [`is_supported`] as `false`; tests ask first and skip the pty-backed
//! assertions there instead of carrying their own host selection.

use std::io;
use std::time::Duration;

/// A pseudo-terminal pair owned by its caller.
///
/// The pair stays live for as long as this value is held: dropping it closes
/// both directions, which is exactly what the deploy-preemption tests need in
/// order to make a port disappear.
pub struct PtyPair {
    inner: super::selected::pty::PtyPair,
}

impl PtyPair {
    /// Open a fresh pseudo-terminal pair on a host that provides one.
    pub fn open() -> io::Result<Self> {
        super::selected::pty::PtyPair::open().map(|inner| Self { inner })
    }

    /// Device name of the slave side (`/dev/pts/7`) — the name a test gives
    /// fbuild as an attached serial port.
    pub fn slave_name(&self) -> &str {
        self.inner.slave_name()
    }

    /// Wait up to `timeout` for bytes the writer on the slave side sent.
    ///
    /// Returns `Ok(false)` when the timeout elapsed with nothing to read.
    pub fn wait_for_master_input(&self, timeout: Duration) -> io::Result<bool> {
        self.inner.wait_for_master_input(timeout)
    }

    /// Read exactly `buffer.len()` bytes the writer sent to the slave side.
    pub fn read_master_exact(&self, buffer: &mut [u8]) -> io::Result<()> {
        self.inner.read_master_exact(buffer)
    }
}

/// Return whether this host can open a pseudo-terminal pair.
pub const fn is_supported() -> bool {
    super::selected::pty::SUPPORTED
}

#[cfg(test)]
mod tests {
    /// The neutral API must either hand back a named slave device or refuse
    /// the request; a silent success with no device name would let a caller
    /// hand fbuild an empty port.
    #[test]
    fn open_returns_a_named_slave_or_an_error() {
        if !super::is_supported() {
            assert!(super::PtyPair::open().is_err());
            return;
        }
        let pty = super::PtyPair::open().expect("open pseudo-terminal");
        let device = std::path::Path::new(pty.slave_name());
        assert!(
            device.is_absolute(),
            "slave name must be an absolute device path: {device:?}"
        );
        assert!(device.exists(), "slave device must exist: {device:?}");
    }
}

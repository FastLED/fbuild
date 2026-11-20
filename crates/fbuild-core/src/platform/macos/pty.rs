//! Pseudo-terminals are absent on this host for fbuild's purposes.
//!
//! macOS pseudo-terminals are allocated through libutil rather than libc on every
//! supported release; that port is unverified here, so this host reports the
//! capability as absent instead of guessing.

use std::io;
use std::time::Duration;

/// This host provides no pseudo-terminal through this capability.
pub const SUPPORTED: bool = false;

/// An unconstructible pair: `PtyPair::open` always fails here, and the
/// opaque field makes that fact part of the type.
pub struct PtyPair {
    _unavailable: std::convert::Infallible,
}

impl PtyPair {
    pub fn open() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "pseudo-terminal test support is unavailable on this host",
        ))
    }

    pub fn slave_name(&self) -> &str {
        match self._unavailable {}
    }

    pub fn wait_for_master_input(&self, _timeout: Duration) -> io::Result<bool> {
        match self._unavailable {}
    }

    pub fn read_master_exact(&self, _buffer: &mut [u8]) -> io::Result<()> {
        match self._unavailable {}
    }
}

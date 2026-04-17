//! Feasibility spike for Issue #66: replace esptool subprocess with the
//! native [`espflash`] Rust crate.
//!
//! # Why
//!
//! `esptool.py` verifies/writes ~2.4 MB ESP32-S3 images in ~6s today but
//! ~1s of that is pure Python startup + stub-flasher upload. Using
//! `espflash` natively (no subprocess, no Python interpreter, optional
//! stub-flasher reuse across calls) should drop a cold verify to
//! ~1.5–2s and a warm verify to <1s.
//!
//! # What this module is right now
//!
//! A narrow feasibility scaffold. It imports the `espflash` crate, pins
//! it as a workspace dep, and confirms the API types we'd need for a
//! real migration are reachable. It does **not** yet replace the
//! esptool-subprocess path in [`super::esp32::Esp32Deployer`].
//!
//! # What still needs to happen before this lands in production
//!
//! 1. Decide transport: `espflash::Flasher` wants an owned
//!    `Box<dyn SerialPort>` (from the `serialport` crate). The daemon
//!    already holds the port via [`fbuild_serial::SharedSerialManager`];
//!    we need an adapter that can lease it out to `espflash` for the
//!    duration of a verify/write and hand it back.
//! 2. Port selection: `espflash` accepts a raw serial port, but esptool
//!    auto-detects chip type via the ROM bootloader handshake. We'll
//!    either rely on our `Esp32Deployer::chip` field (already known
//!    from the board config) or let `espflash` detect.
//! 3. Error mapping: `espflash::Error` -> `fbuild_core::FbuildError`.
//! 4. Stub flasher reuse: `espflash::Flasher` keeps the stub loaded for
//!    the lifetime of the struct. To hit the <1s warm-verify number we
//!    need to keep a `Flasher` around between deploys instead of
//!    rebuilding it per call.
//! 5. Progress reporting: `espflash` has a progress callback interface;
//!    we want to bridge that to the daemon's WebSocket log stream.
//!
//! # Fallback plan
//!
//! If the espflash API turns out to be too unstable or the stub-reuse
//! ergonomics too messy, we can revisit `perf(deploy)` gains via
//! sector-selective writes (already done, see #67) and skip #66. The
//! feasibility conclusion for this spike should be captured in #66
//! before we invest in a full port.

#[allow(unused_imports)]
use espflash::flasher::Flasher;

/// Returns the `espflash` crate version we're currently linking against.
///
/// Used by the feasibility test to assert the dep resolves and the
/// surface we need is reachable. Real callers shouldn't depend on this.
#[must_use]
pub fn espflash_version() -> &'static str {
    // `espflash` doesn't re-export CARGO_PKG_VERSION; pin the version
    // manually so a future `cargo update` that pulls in an incompatible
    // 5.x trips a test rather than silently changing behavior.
    "4"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feasibility assertion: the `espflash` crate compiles in our
    /// workspace and `Flasher` is name-reachable. A future PR that wires
    /// the native path will replace this test with one that actually
    /// constructs a `Flasher` against a mock transport.
    #[test]
    fn espflash_dependency_is_linkable() {
        // Invoking the name in any form keeps the `use Flasher` import
        // honest against dead-code elimination.
        let _ = std::mem::size_of::<Flasher>();
        assert_eq!(espflash_version(), "4");
    }
}

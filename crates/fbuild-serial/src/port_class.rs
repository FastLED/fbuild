//! Caller-facing kernel-driver classification of a serial port.
//!
//! FastLED/fbuild#895. The detection mechanics (Linux sysfs driver-symlink
//! reads, macOS device-node naming, Windows deferred SetupDi) live behind
//! [`fbuild_core::platform::device::detect_serial_kernel_driver`]; this
//! module keeps only the caller-facing name and enum so existing consumers
//! (`boards.rs`, the daemon device manager, `port scan`) are unchanged.

/// The kernel's view of which driver class instantiated this port.
///
/// Re-exported from the platform facade; see the facade docs for the
/// per-platform detection strategy and the safety contract (ambiguous
/// cases yield `None`, callers fall through to their existing defaults).
pub use fbuild_core::platform::device::KernelDriverClass as PortKernelClass;

/// Detect the port's kernel-side driver class.
///
/// Returns `None` if the port can't be classified (already
/// disconnected, virtual port, container without sysfs/IOReg,
/// unsupported platform). Callers MUST fall through to their existing
/// default on `None` — this function is purely additive.
#[must_use]
pub fn detect_port_kernel_class(port_name: &str) -> Option<PortKernelClass> {
    fbuild_core::platform::device::detect_serial_kernel_driver(port_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_delegates_to_the_platform_facade() {
        // Pure delegation: whatever the facade says for a name that no
        // host classifies (a Windows-style COM name on any host, and a
        // nonexistent tty on Linux/macOS) is exactly None. Pins the
        // shim's contract without duplicating the per-OS tests that
        // live next to each selected implementation.
        assert_eq!(detect_port_kernel_class("COM3"), None);
        assert_eq!(detect_port_kernel_class("/dev/does-not-exist-ttyX"), None);
    }
}

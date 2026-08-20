//! Lease-reaping adapter for the neutral process facade.

/// Check whether a recorded lease owner is still alive.
pub(super) fn is_pid_alive(pid: u32) -> bool {
    fbuild_core::platform::process::pid_is_alive(pid)
}

//! Neutral host-platform facade.
//!
//! This module is the only production site that selects the operating system.
//! Callers use the capability modules and cannot name the private concrete
//! implementation selected below.

pub mod device;
pub mod executable;
pub mod fs;
pub mod host;
pub mod ipc;
pub mod process;

std::cfg_select! {
    target_os = "windows" => {
        #[path = "windows/mod.rs"]
        mod selected;
    }
    target_os = "linux" => {
        #[path = "linux/mod.rs"]
        mod selected;
    }
    target_os = "macos" => {
        #[path = "macos/mod.rs"]
        mod selected;
    }
}

/// Return the host operating system selected for this fbuild executable.
pub const fn current_os() -> host::HostOs {
    selected::HOST_OS
}

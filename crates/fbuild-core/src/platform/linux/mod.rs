use super::host::{HostArch, HostOs};
use crate::path::NormalizedPath;

pub(super) mod ape;
pub(super) mod device;
pub(super) mod fs;
pub(super) mod host;
pub(super) mod ipc;
mod owner_thread;
pub(super) mod process;
pub(super) mod pty;
pub(super) mod runtime;
pub(super) mod usb_pnp;

pub(super) const HOST_OS: HostOs = HostOs::Linux;

/// The kernel's own handle on the running image. It keeps naming the loaded
/// inode after the launch path is replaced, renamed, or unlinked.
pub(super) fn replace_proof_live_image() -> Option<NormalizedPath> {
    Some(NormalizedPath::from(std::path::Path::new("/proc/self/exe")))
}

pub(super) fn host_arch() -> HostArch {
    match std::env::consts::ARCH {
        "x86" => HostArch::X86,
        "x86_64" => HostArch::X86_64,
        "arm" => HostArch::Arm,
        "aarch64" => HostArch::Aarch64,
        _ => HostArch::Other,
    }
}

use super::host::{HostArch, HostOs};
use crate::path::NormalizedPath;

pub(super) mod ape;
pub(super) mod device;
pub(super) mod fs;
pub(super) mod host;
pub(super) mod ipc;
pub(super) mod process;
pub(super) mod usb_pnp;

pub(super) const HOST_OS: HostOs = HostOs::Macos;

/// This host exposes no handle that survives replacement of the launch
/// path, so callers must judge the path they were given.
pub(super) fn replace_proof_live_image() -> Option<NormalizedPath> {
    None
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

use super::host::{HostArch, HostOs};

pub(super) mod device;
pub(super) mod fs;
pub(super) mod ipc;
pub(super) mod process;
pub(super) mod usb_pnp;

pub(super) const HOST_OS: HostOs = HostOs::Macos;

pub(super) fn host_arch() -> HostArch {
    match std::env::consts::ARCH {
        "x86" => HostArch::X86,
        "x86_64" => HostArch::X86_64,
        "arm" => HostArch::Arm,
        "aarch64" => HostArch::Aarch64,
        _ => HostArch::Other,
    }
}

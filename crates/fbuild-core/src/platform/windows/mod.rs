use super::host::{HostArch, HostOs};

pub(super) const HOST_OS: HostOs = HostOs::Windows;

pub(super) fn host_arch() -> HostArch {
    match std::env::consts::ARCH {
        "x86" => HostArch::X86,
        "x86_64" => HostArch::X86_64,
        "arm" => HostArch::Arm,
        "aarch64" => HostArch::Aarch64,
        _ => HostArch::Other,
    }
}

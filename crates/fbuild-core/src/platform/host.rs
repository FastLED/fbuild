//! Neutral host identity and runtime facts.
//!
//! Host-filesystem mechanics (e.g. the home directory) live in the
//! per-OS `selected::host` tree; this module exposes the neutral API.

/// Operating systems supported by the fbuild executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostOs {
    Windows,
    Linux,
    Macos,
}

/// Architectures used when selecting host artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostArch {
    X86,
    X86_64,
    Arm,
    Aarch64,
    Other,
}

/// The operating system and CPU architecture of the machine running fbuild.
///
/// This is deliberately separate from every embedded board/compiler target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostPlatform {
    os: HostOs,
    arch: HostArch,
}

impl HostPlatform {
    pub const fn new(os: HostOs, arch: HostArch) -> Self {
        Self { os, arch }
    }

    pub const fn os(self) -> HostOs {
        self.os
    }

    pub const fn arch(self) -> HostArch {
        self.arch
    }

    pub const fn is_windows(self) -> bool {
        matches!(self.os, HostOs::Windows)
    }

    pub const fn is_linux(self) -> bool {
        matches!(self.os, HostOs::Linux)
    }

    pub const fn is_macos(self) -> bool {
        matches!(self.os, HostOs::Macos)
    }

    pub const fn is_unix(self) -> bool {
        matches!(self.os, HostOs::Linux | HostOs::Macos)
    }

    pub const fn os_name(self) -> &'static str {
        match self.os {
            HostOs::Windows => "windows",
            HostOs::Linux => "linux",
            HostOs::Macos => "macos",
        }
    }

    pub const fn arch_name(self) -> &'static str {
        match self.arch {
            HostArch::X86 => "x86",
            HostArch::X86_64 => "x86_64",
            HostArch::Arm => "arm",
            HostArch::Aarch64 => "aarch64",
            HostArch::Other => "other",
        }
    }

    pub const fn path_list_separator(self) -> char {
        if self.is_windows() { ';' } else { ':' }
    }

    pub const fn path_list_separator_str(self) -> &'static str {
        if self.is_windows() { ";" } else { ":" }
    }
}

/// Return facts for the machine running this fbuild executable.
pub fn current() -> HostPlatform {
    HostPlatform::new(super::current_os(), super::selected::host_arch())
}

pub const fn current_os() -> HostOs {
    super::current_os()
}

pub const fn is_windows() -> bool {
    matches!(current_os(), HostOs::Windows)
}

pub const fn is_linux() -> bool {
    matches!(current_os(), HostOs::Linux)
}

pub const fn is_macos() -> bool {
    matches!(current_os(), HostOs::Macos)
}

pub const fn is_unix() -> bool {
    matches!(current_os(), HostOs::Linux | HostOs::Macos)
}

pub fn os_name() -> &'static str {
    current().os_name()
}

pub fn arch_name() -> &'static str {
    current().arch_name()
}

pub fn path_list_separator() -> char {
    current().path_list_separator()
}

pub fn path_list_separator_str() -> &'static str {
    current().path_list_separator_str()
}

/// The current user's home directory: `%USERPROFILE%` on Windows (with a
/// `%HOME%` fallback for POSIX-style shells), `$HOME` elsewhere.
///
/// Callers keep their own policy for what to do when the variable is
/// unset — the facade only reports the fact.
pub fn home_dir() -> Option<std::path::PathBuf> {
    super::selected::host::home_dir()
}

#[cfg(test)]
mod tests {
    use super::{HostArch, HostOs, HostPlatform};

    #[test]
    fn explicit_host_platform_keeps_os_arch_and_path_separator_together() {
        let windows = HostPlatform::new(HostOs::Windows, HostArch::X86_64);
        let linux = HostPlatform::new(HostOs::Linux, HostArch::Aarch64);
        let macos = HostPlatform::new(HostOs::Macos, HostArch::Arm);

        assert_eq!(windows.os_name(), "windows");
        assert_eq!(windows.arch_name(), "x86_64");
        assert_eq!(windows.path_list_separator(), ';');
        assert_eq!(linux.os_name(), "linux");
        assert_eq!(linux.arch_name(), "aarch64");
        assert_eq!(linux.path_list_separator(), ':');
        assert_eq!(macos.os_name(), "macos");
        assert_eq!(macos.arch_name(), "arm");
        assert_eq!(macos.path_list_separator(), ':');
    }

    #[test]
    fn current_host_is_a_supported_os_with_a_named_architecture() {
        let current = super::current();
        assert!(matches!(
            current.os(),
            HostOs::Windows | HostOs::Linux | HostOs::Macos
        ));
        assert!(!current.arch_name().is_empty());
    }
}

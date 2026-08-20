//! Neutral host identity and runtime facts.

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

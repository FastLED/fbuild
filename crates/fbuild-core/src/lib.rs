//! Core types, errors, and utilities for fbuild.
//!
//! This crate provides the foundational types shared across all fbuild crates:
//! - Error types (FbuildError)
//! - Build profiles and enums
//! - Subprocess runner with platform-specific flags
//! - Size info parsing (avr-size / arm-none-eabi-size output)

pub mod build_log;
pub mod compiler_flags;
pub mod containment;
pub mod elapsed;
pub mod emulator;
pub mod install_status;
pub mod path;
pub mod response_file;
pub mod shell_split;
pub mod subprocess;
pub mod symbol_analysis;
pub mod usb;

pub use build_log::BuildLog;

use serde::{Deserialize, Serialize};

/// Top-level error type for fbuild operations.
#[derive(Debug, thiserror::Error)]
pub enum FbuildError {
    #[error("build failed: {0}")]
    BuildFailed(String),

    #[error("deploy failed: {0}")]
    DeployFailed(String),

    #[error("serial error: {0}")]
    SerialError(String),

    #[error("config error: {0}")]
    ConfigError(String),

    #[error("package error: {0}")]
    PackageError(String),

    #[error("daemon error: {0}")]
    DaemonError(String),

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, FbuildError>;

/// Build profile selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildProfile {
    Release,
    Quick,
}

impl BuildProfile {
    /// Directory name used for build output (e.g. `.fbuild/build/{env}/{profile}/`).
    pub fn as_dir_name(&self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Quick => "quick",
        }
    }
}

/// Platform identifier for orchestrator dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Apollo3,
    AtmelAvr,
    AtmelMegaAvr,
    AtmelSam,
    Ch32v,
    Espressif32,
    Espressif8266,
    NordicNrf52,
    NxpLpc,
    RaspberryPi,
    RenesasRa,
    SiliconLabs,
    Ststm32,
    Teensy,
    Wasm,
}

impl Platform {
    /// Parse a platform string from platformio.ini.
    ///
    /// Uses substring matching to handle all PlatformIO platform spec forms:
    /// bare names, owner-prefixed, versioned, git URLs, git refs, local paths.
    pub fn from_platform_str(s: &str) -> Option<Self> {
        let s = s.to_lowercase();
        // Apollo3 (Ambiq Micro) — check before generic substring matches.
        if s.contains("apollo3") {
            Some(Self::Apollo3)
        // Check espressif8266 before espressif32 to avoid false match
        // ("espressif8266" does not contain "espressif32", but be explicit).
        } else if s.contains("espressif8266") {
            Some(Self::Espressif8266)
        } else if s.contains("espressif32") {
            Some(Self::Espressif32)
        } else if s.contains("atmelmegaavr") {
            Some(Self::AtmelMegaAvr)
        } else if s.contains("atmelsam") {
            Some(Self::AtmelSam)
        } else if s.contains("atmelavr") {
            Some(Self::AtmelAvr)
        } else if s.contains("ch32v") {
            Some(Self::Ch32v)
        } else if s.contains("nordicnrf52") {
            Some(Self::NordicNrf52)
        } else if s.contains("nxplpc") {
            Some(Self::NxpLpc)
        } else if s.contains("renesas") {
            Some(Self::RenesasRa)
        } else if s.contains("siliconlabs") {
            Some(Self::SiliconLabs)
        } else if s.contains("ststm32") {
            Some(Self::Ststm32)
        } else if s.contains("raspberrypi") {
            Some(Self::RaspberryPi)
        } else if s.contains("teensy") {
            Some(Self::Teensy)
        } else if s.contains("emscripten") || s.contains("wasm") {
            Some(Self::Wasm)
        } else {
            None
        }
    }

    /// Check if a raw platform string identifies this platform.
    pub fn matches_str(&self, s: &str) -> bool {
        Platform::from_platform_str(s) == Some(*self)
    }
}

/// Operation types the daemon can process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    Build,
    Deploy,
    Monitor,
    BuildAndDeploy,
    InstallDependencies,
}

/// Daemon state.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    #[default]
    Idle,
    Building,
    Deploying,
    Monitoring,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

impl std::fmt::Display for DaemonState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Building => write!(f, "building"),
            Self::Deploying => write!(f, "deploying"),
            Self::Monitoring => write!(f, "monitoring"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Size information after a successful build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizeInfo {
    pub text: u64,
    pub data: u64,
    pub bss: u64,
    pub total_flash: u64,
    pub total_ram: u64,
    pub max_flash: Option<u64>,
    pub max_ram: Option<u64>,
}

impl SizeInfo {
    pub fn flash_percent(&self) -> Option<f64> {
        self.max_flash
            .map(|max| (self.total_flash as f64 / max as f64) * 100.0)
    }

    pub fn ram_percent(&self) -> Option<f64> {
        self.max_ram
            .map(|max| (self.total_ram as f64 / max as f64) * 100.0)
    }

    /// Parse size output from avr-size or arm-none-eabi-size.
    ///
    /// Supports two formats:
    /// 1. Berkeley format: `text  data  bss  dec  hex  filename`
    /// 2. AVR section format: `.section  size  address`
    pub fn parse(output: &str, max_flash: Option<u64>, max_ram: Option<u64>) -> Option<Self> {
        // Try Berkeley format first: look for a line with numeric columns
        // Format: "   text    data     bss     dec     hex filename"
        //         "   1234     56      78    1368     558 firmware.elf"
        for line in output.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                if let (Ok(text), Ok(data), Ok(bss)) = (
                    parts[0].parse::<u64>(),
                    parts[1].parse::<u64>(),
                    parts[2].parse::<u64>(),
                ) {
                    return Some(Self {
                        text,
                        data,
                        bss,
                        total_flash: text + data,
                        total_ram: data + bss,
                        max_flash,
                        max_ram,
                    });
                }
            }
        }

        // Try AVR section format: ".text  1234  0x0"
        let mut text = 0u64;
        let mut data = 0u64;
        let mut bss = 0u64;
        let mut found = false;

        for line in output.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(size) = parts[1].parse::<u64>() {
                    match parts[0] {
                        ".text" => {
                            text = size;
                            found = true;
                        }
                        ".data" => {
                            data = size;
                            found = true;
                        }
                        ".bss" => {
                            bss = size;
                            found = true;
                        }
                        _ => {}
                    }
                }
            }
        }

        if found {
            Some(Self {
                text,
                data,
                bss,
                total_flash: text + data,
                total_ram: data + bss,
                max_flash,
                max_ram,
            })
        } else {
            None
        }
    }
}

/// Memory region classification for a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryRegion {
    Flash,
    Ram,
}

/// A single symbol from the ELF binary with its memory classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub name: String,
    pub size: u64,
    pub region: MemoryRegion,
    /// nm type character (T, D, B, R, etc.)
    pub sym_type: char,
}

/// Per-symbol breakdown of firmware memory usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolMap {
    pub symbols: Vec<SymbolEntry>,
    pub total_flash: u64,
    pub total_ram: u64,
}

impl SymbolMap {
    /// Classify an `nm` type character into a memory region.
    ///
    /// Returns `None` for symbols that don't map to Flash or RAM
    /// (undefined, absolute, debug, etc.).
    fn classify(sym_type: char) -> Option<MemoryRegion> {
        match sym_type {
            // Flash: text (code), read-only data, weak symbols in text
            'T' | 't' | 'R' | 'r' | 'W' | 'w' => Some(MemoryRegion::Flash),
            // RAM: initialized data, uninitialized data (BSS)
            'D' | 'd' | 'B' | 'b' => Some(MemoryRegion::Ram),
            // Everything else: undefined, absolute, debug, etc.
            _ => None,
        }
    }

    /// Parse output from `nm --print-size --size-sort --reverse-sort`.
    ///
    /// Expected format per line: `<address> <size> <type> <name>`
    /// Example: `00000080 00000024 T setup`
    pub fn parse_nm_output(output: &str) -> Option<Self> {
        let mut symbols = Vec::new();
        let mut total_flash = 0u64;
        let mut total_ram = 0u64;

        for line in output.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 {
                continue;
            }

            // parts[0] = address (hex), parts[1] = size (hex), parts[2] = type, parts[3..] = name
            let size = match u64::from_str_radix(parts[1], 16) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let sym_type_char = match parts[2].chars().next() {
                Some(c) => c,
                None => continue,
            };

            let region = match Self::classify(sym_type_char) {
                Some(r) => r,
                None => continue,
            };

            // Name may contain spaces (e.g. C++ templates), rejoin
            let name = parts[3..].join(" ");
            if size == 0 {
                continue;
            }

            match region {
                MemoryRegion::Flash => total_flash += size,
                MemoryRegion::Ram => total_ram += size,
            }

            symbols.push(SymbolEntry {
                name,
                size,
                region,
                sym_type: sym_type_char,
            });
        }

        if symbols.is_empty() {
            return None;
        }

        Some(Self {
            symbols,
            total_flash,
            total_ram,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_info_percentages() {
        let info = SizeInfo {
            text: 1000,
            data: 200,
            bss: 100,
            total_flash: 1200,
            total_ram: 300,
            max_flash: Some(32768),
            max_ram: Some(2048),
        };
        assert!(info.flash_percent().unwrap() < 4.0);
        assert!(info.ram_percent().unwrap() < 15.0);
    }

    #[test]
    fn size_info_no_max() {
        let info = SizeInfo {
            text: 1000,
            data: 200,
            bss: 100,
            total_flash: 1200,
            total_ram: 300,
            max_flash: None,
            max_ram: None,
        };
        assert!(info.flash_percent().is_none());
        assert!(info.ram_percent().is_none());
    }

    #[test]
    fn size_info_parse_berkeley() {
        let output = "   text\t   data\t    bss\t    dec\t    hex\tfilename\n   \
                       924\t     14\t      9\t    947\t    3b3\tfirmware.elf\n";
        let info = SizeInfo::parse(output, Some(32256), Some(2048)).unwrap();
        assert_eq!(info.text, 924);
        assert_eq!(info.data, 14);
        assert_eq!(info.bss, 9);
        assert_eq!(info.total_flash, 938);
        assert_eq!(info.total_ram, 23);
    }

    #[test]
    fn size_info_parse_avr_sections() {
        let output = ".text   924   0x0\n.data   14   0x800100\n.bss   9   0x80010e\n";
        let info = SizeInfo::parse(output, Some(32256), Some(2048)).unwrap();
        assert_eq!(info.text, 924);
        assert_eq!(info.data, 14);
        assert_eq!(info.bss, 9);
    }

    #[test]
    fn size_info_parse_garbage_returns_none() {
        let output = "some random garbage\nnot a size output\n";
        assert!(SizeInfo::parse(output, None, None).is_none());
    }

    #[test]
    fn platform_from_str_bare_names() {
        assert_eq!(
            Platform::from_platform_str("atmelavr"),
            Some(Platform::AtmelAvr)
        );
        assert_eq!(
            Platform::from_platform_str("espressif32"),
            Some(Platform::Espressif32)
        );
        assert_eq!(
            Platform::from_platform_str("espressif8266"),
            Some(Platform::Espressif8266)
        );
        assert_eq!(
            Platform::from_platform_str("raspberrypi"),
            Some(Platform::RaspberryPi)
        );
        assert_eq!(
            Platform::from_platform_str("raspberrypipico"),
            Some(Platform::RaspberryPi)
        );
        assert_eq!(
            Platform::from_platform_str("ststm32"),
            Some(Platform::Ststm32)
        );
        assert_eq!(
            Platform::from_platform_str("teensy"),
            Some(Platform::Teensy)
        );
        assert_eq!(Platform::from_platform_str("wasm"), Some(Platform::Wasm));
        assert_eq!(
            Platform::from_platform_str("emscripten"),
            Some(Platform::Wasm)
        );
    }

    #[test]
    fn platform_from_str_owner_prefixed() {
        assert_eq!(
            Platform::from_platform_str("platformio/espressif32"),
            Some(Platform::Espressif32)
        );
        assert_eq!(
            Platform::from_platform_str("platformio/atmelavr"),
            Some(Platform::AtmelAvr)
        );
    }

    #[test]
    fn platform_from_str_versioned() {
        assert_eq!(
            Platform::from_platform_str("platformio/espressif32@^6.3.0"),
            Some(Platform::Espressif32)
        );
        assert_eq!(
            Platform::from_platform_str("espressif32@6.3.0"),
            Some(Platform::Espressif32)
        );
    }

    #[test]
    fn platform_from_str_git_urls() {
        assert_eq!(
            Platform::from_platform_str("https://github.com/platformio/platform-espressif32.git"),
            Some(Platform::Espressif32)
        );
        assert_eq!(
            Platform::from_platform_str(
                "https://github.com/platformio/platform-espressif32.git#v6.3.0"
            ),
            Some(Platform::Espressif32)
        );
        assert_eq!(
            Platform::from_platform_str(
                "https://github.com/platformio/platform-atmelavr.git#feature-branch"
            ),
            Some(Platform::AtmelAvr)
        );
    }

    #[test]
    fn platform_from_str_local_paths() {
        assert_eq!(
            Platform::from_platform_str("/home/user/platform-espressif32"),
            Some(Platform::Espressif32)
        );
        assert_eq!(
            Platform::from_platform_str("../platform-teensy"),
            Some(Platform::Teensy)
        );
    }

    #[test]
    fn platform_from_str_case_insensitive() {
        assert_eq!(
            Platform::from_platform_str("Espressif32"),
            Some(Platform::Espressif32)
        );
        assert_eq!(
            Platform::from_platform_str("ATMELAVR"),
            Some(Platform::AtmelAvr)
        );
        assert_eq!(
            Platform::from_platform_str("TEENSY"),
            Some(Platform::Teensy)
        );
    }

    #[test]
    fn platform_from_str_ch32v() {
        assert_eq!(Platform::from_platform_str("ch32v"), Some(Platform::Ch32v));
        assert_eq!(
            Platform::from_platform_str("platformio/ch32v"),
            Some(Platform::Ch32v)
        );
        assert_eq!(Platform::from_platform_str("CH32V"), Some(Platform::Ch32v));
    }

    #[test]
    fn platform_from_str_nxplpc() {
        assert_eq!(
            Platform::from_platform_str("nxplpc"),
            Some(Platform::NxpLpc)
        );
        assert_eq!(
            Platform::from_platform_str("platformio/nxplpc"),
            Some(Platform::NxpLpc)
        );
        assert_eq!(
            Platform::from_platform_str("NXPLPC"),
            Some(Platform::NxpLpc)
        );
    }

    #[test]
    fn platform_from_str_unknown() {
        assert_eq!(Platform::from_platform_str("unknown"), None);
        assert_eq!(Platform::from_platform_str(""), None);
        assert_eq!(Platform::from_platform_str("nrf52"), None);
    }

    #[test]
    fn platform_from_str_no_cross_match() {
        // espressif8266 must NOT match Espressif32
        assert_ne!(
            Platform::from_platform_str("espressif8266"),
            Some(Platform::Espressif32)
        );
        // espressif32 must NOT match Espressif8266
        assert_ne!(
            Platform::from_platform_str("espressif32"),
            Some(Platform::Espressif8266)
        );
    }

    #[test]
    fn platform_matches_str() {
        assert!(Platform::Espressif32.matches_str("espressif32"));
        assert!(Platform::Espressif32.matches_str("platformio/espressif32@^6.3.0"));
        assert!(!Platform::Espressif32.matches_str("espressif8266"));
        assert!(!Platform::AtmelAvr.matches_str("teensy"));
    }

    #[test]
    fn platform_from_str_atmelmegaavr() {
        assert_eq!(
            Platform::from_platform_str("atmelmegaavr"),
            Some(Platform::AtmelMegaAvr)
        );
        assert_eq!(
            Platform::from_platform_str("platformio/atmelmegaavr"),
            Some(Platform::AtmelMegaAvr)
        );
        assert_eq!(
            Platform::from_platform_str("ATMELMEGAAVR"),
            Some(Platform::AtmelMegaAvr)
        );
    }

    #[test]
    fn platform_atmelmegaavr_not_atmelavr() {
        // "atmelmegaavr" contains "atmelavr" as substring — must NOT match AtmelAvr
        assert_ne!(
            Platform::from_platform_str("atmelmegaavr"),
            Some(Platform::AtmelAvr)
        );
    }

    #[test]
    fn symbol_map_parse_nm_output_basic() {
        let output = "\
00000080 00000024 T setup\n\
000000a4 00000012 T loop\n\
00000200 00000008 D some_data\n\
00000300 00000010 B bss_buffer\n\
00000400 00000006 R const_table\n";
        let map = SymbolMap::parse_nm_output(output).unwrap();
        assert_eq!(map.symbols.len(), 5);
        // Flash = setup(0x24=36) + loop(0x12=18) + const_table(0x06=6) = 60
        assert_eq!(map.total_flash, 60);
        // RAM = some_data(0x08=8) + bss_buffer(0x10=16) = 24
        assert_eq!(map.total_ram, 24);
    }

    #[test]
    fn symbol_map_parse_nm_output_skips_undefined() {
        let output = "\
00000080 00000024 T setup\n\
         U _start\n\
00000200 00000008 D data_var\n";
        let map = SymbolMap::parse_nm_output(output).unwrap();
        assert_eq!(map.symbols.len(), 2);
    }

    #[test]
    fn symbol_map_parse_nm_output_empty() {
        assert!(SymbolMap::parse_nm_output("").is_none());
        assert!(SymbolMap::parse_nm_output("garbage\nnot nm output\n").is_none());
    }

    #[test]
    fn symbol_map_parse_nm_output_skips_zero_size() {
        let output = "00000080 00000000 T empty_func\n00000080 00000010 T real_func\n";
        let map = SymbolMap::parse_nm_output(output).unwrap();
        assert_eq!(map.symbols.len(), 1);
        assert_eq!(map.symbols[0].name, "real_func");
    }

    #[test]
    fn symbol_map_classify() {
        assert_eq!(SymbolMap::classify('T'), Some(MemoryRegion::Flash));
        assert_eq!(SymbolMap::classify('t'), Some(MemoryRegion::Flash));
        assert_eq!(SymbolMap::classify('R'), Some(MemoryRegion::Flash));
        assert_eq!(SymbolMap::classify('r'), Some(MemoryRegion::Flash));
        assert_eq!(SymbolMap::classify('W'), Some(MemoryRegion::Flash));
        assert_eq!(SymbolMap::classify('D'), Some(MemoryRegion::Ram));
        assert_eq!(SymbolMap::classify('d'), Some(MemoryRegion::Ram));
        assert_eq!(SymbolMap::classify('B'), Some(MemoryRegion::Ram));
        assert_eq!(SymbolMap::classify('b'), Some(MemoryRegion::Ram));
        assert_eq!(SymbolMap::classify('U'), None);
        assert_eq!(SymbolMap::classify('A'), None);
        assert_eq!(SymbolMap::classify('N'), None);
    }

    /// Guard: .env must contain only safe PATH entries, never secrets.
    #[test]
    fn dotenv_contains_only_path() {
        // Walk up from the crate directory to find the workspace root .env
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root");
        let env_path = workspace_root.join(".env");
        let contents = std::fs::read_to_string(&env_path)
            .unwrap_or_else(|e| panic!(".env not found at {}: {e}", env_path.display()));

        // Only allowed variables (whitelist)
        let allowed_keys = ["PATH"];

        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let key = line
                .split('=')
                .next()
                .expect("each .env line must be KEY=VALUE");
            assert!(
                allowed_keys.contains(&key),
                ".env contains disallowed key {key:?} — only {allowed_keys:?} are permitted. \
                 Do not commit secrets to .env!"
            );
        }
    }
}

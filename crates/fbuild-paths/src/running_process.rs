//! Minimal `running-process` broker adoption seam for fbuild.
//!
//! The current fbuild release still talks to `fbuild-daemon` over its direct
//! loopback HTTP endpoint. This module records the broker-facing service
//! metadata and escape-hatch behavior so future `connect_to_backend` wiring can
//! replace the direct path without scattering policy through CLI/PyO3 callers.

use std::path::{Path, PathBuf};

pub const SERVICE_NAME: &str = "fbuild";
pub const SERVICE_DEFINITION_FILE_NAME: &str = "fbuild.servicedef";
pub const SERVICE_DEFINITION_TEMPLATE: &str =
    "crates/fbuild-daemon/running-process/fbuild-daemon.servicedef.textproto.in";
pub const BROKER_ISOLATION: &str = "SHARED_BROKER";

pub const RUNNING_PROCESS_DISABLE_ENV: &str = "RUNNING_PROCESS_DISABLE";
pub const RUNNING_PROCESS_SERVICE_DEF_DIR_ENV: &str = "RUNNING_PROCESS_SERVICE_DEF_DIR";
pub const FBUILD_RUNNING_PROCESS_BROKER_ENV: &str = "FBUILD_RUNNING_PROCESS_BROKER";

#[cfg(windows)]
pub const DAEMON_BINARY_NAME: &str = "fbuild-daemon.exe";
#[cfg(not(windows))]
pub const DAEMON_BINARY_NAME: &str = "fbuild-daemon";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningProcessDaemonMode {
    /// Use the existing direct HTTP daemon path.
    DirectFallback,
    /// Broker mode was explicitly requested, but the broker client is stubbed
    /// until FastLED/fbuild#510 lands the real `connect_to_backend` path.
    BrokerRequested,
}

impl RunningProcessDaemonMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirectFallback => "direct-fallback",
            Self::BrokerRequested => "broker-requested-direct-fallback",
        }
    }

    pub fn uses_direct_fallback(self) -> bool {
        true
    }
}

pub fn running_process_disabled() -> bool {
    env_flag_is_one(RUNNING_PROCESS_DISABLE_ENV)
}

pub fn running_process_broker_requested() -> bool {
    !running_process_disabled() && env_flag_is_one(FBUILD_RUNNING_PROCESS_BROKER_ENV)
}

pub fn running_process_daemon_mode() -> RunningProcessDaemonMode {
    if running_process_broker_requested() {
        RunningProcessDaemonMode::BrokerRequested
    } else {
        RunningProcessDaemonMode::DirectFallback
    }
}

pub fn running_process_adoption_summary() -> &'static str {
    if running_process_disabled() {
        "direct daemon fallback (RUNNING_PROCESS_DISABLE=1)"
    } else if running_process_broker_requested() {
        "broker requested; direct daemon fallback until FastLED/fbuild#510 wires connect_to_backend"
    } else {
        "direct daemon fallback (running-process broker client stubbed)"
    }
}

pub fn running_process_service_definition_dir() -> PathBuf {
    if let Some(path) = std::env::var_os(RUNNING_PROCESS_SERVICE_DEF_DIR_ENV) {
        return PathBuf::from(path);
    }
    platform_service_definition_dir()
}

pub fn running_process_service_definition_path() -> PathBuf {
    running_process_service_definition_path_in(running_process_service_definition_dir())
}

pub fn running_process_service_definition_path_in(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref().join(SERVICE_DEFINITION_FILE_NAME)
}

fn env_flag_is_one(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1")
}

#[cfg(windows)]
fn platform_service_definition_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|home| PathBuf::from(home).join("AppData").join("Roaming"))
        })
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("running-process")
        .join("services")
}

#[cfg(target_os = "macos")]
fn platform_service_definition_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Library")
        .join("Application Support")
        .join("running-process")
        .join("services")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_service_definition_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("running-process")
        .join("services")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_definition_metadata_matches_tracker() {
        assert_eq!(SERVICE_NAME, "fbuild");
        assert_eq!(SERVICE_DEFINITION_FILE_NAME, "fbuild.servicedef");
        assert_eq!(BROKER_ISOLATION, "SHARED_BROKER");
        assert!(SERVICE_DEFINITION_TEMPLATE.ends_with(".textproto.in"));
    }

    #[test]
    fn service_definition_path_uses_frozen_filename() {
        let root = PathBuf::from("/tmp/running-process/services");
        assert_eq!(
            running_process_service_definition_path_in(&root),
            root.join(SERVICE_DEFINITION_FILE_NAME)
        );
    }

    #[test]
    fn broker_requested_mode_is_still_direct_fallback_for_this_slice() {
        let mode = RunningProcessDaemonMode::BrokerRequested;
        assert_eq!(mode.as_str(), "broker-requested-direct-fallback");
        assert!(mode.uses_direct_fallback());
    }
}

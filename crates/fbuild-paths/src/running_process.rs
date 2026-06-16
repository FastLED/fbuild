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

/// The trust-group label CI uses for `EXPLICIT_INSTANCE` isolation.
pub const CI_TRUSTED_INSTANCE: &str = "ci-trusted";

/// Minimum acceptable fbuild backend version the broker will negotiate.
pub const MIN_VERSION: &str = "1.0.0";

/// fbuild's registered v1 broker payload-protocol ID (registered-consumer range
/// `0x7000..=0x7EFF`). The authoritative compile-time pin lives in
/// `fbuild-daemon`'s broker module via `running_process::register_payload_protocol!`;
/// this plain copy is the value the CLI diagnostic prints without pulling in the
/// `running-process` dependency. A drift test in the daemon asserts the two agree.
pub const FBUILD_PAYLOAD_PROTOCOL: u32 = 0x7EB1;

/// fbuild's internal request/response payload-schema version (bumped
/// independently of the running-process broker envelope version).
pub const FBUILD_PROTOCOL_VERSION: u32 = 1;

pub const RUNNING_PROCESS_DISABLE_ENV: &str = "RUNNING_PROCESS_DISABLE";
pub const RUNNING_PROCESS_SERVICE_DEF_DIR_ENV: &str = "RUNNING_PROCESS_SERVICE_DEF_DIR";
pub const FBUILD_RUNNING_PROCESS_BROKER_ENV: &str = "FBUILD_RUNNING_PROCESS_BROKER";
pub const FBUILD_CACHE_DIR_ENV: &str = "FBUILD_CACHE_DIR";
pub const LOCAL_TRUST_DOMAIN: &str = "local-shared";

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

/// Broker/cache identity for local fbuild daemons.
///
/// This is the policy boundary for large shared artifacts. Backend version is
/// deliberately not part of the identity: package, toolchain, framework, and
/// sidecar artifacts are owned by the canonical cache root and trust domain,
/// not by whichever fbuild daemon version the broker negotiates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonCacheIdentity {
    pub mode: &'static str,
    pub cache_root: PathBuf,
    pub cache_root_key: String,
    pub cache_dir_source: &'static str,
    pub trust_domain: &'static str,
}

impl DaemonCacheIdentity {
    pub fn discover() -> Self {
        let cache_root = crate::get_cache_root();
        Self {
            mode: if crate::is_dev_mode() { "dev" } else { "prod" },
            cache_root_key: stable_path_key(&cache_root),
            cache_dir_source: if std::env::var_os(FBUILD_CACHE_DIR_ENV).is_some() {
                FBUILD_CACHE_DIR_ENV
            } else {
                "default"
            },
            cache_root,
            trust_domain: LOCAL_TRUST_DOMAIN,
        }
    }

    pub fn label_value(&self) -> String {
        format!(
            "mode={};trust={};cache={}",
            self.mode, self.trust_domain, self.cache_root_key
        )
    }
}

fn stable_path_key(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    absolute.to_string_lossy().replace('\\', "/")
}

/// The seven cache roots fbuild records in its broker manifest, resolved from
/// this crate (the single source of truth for fbuild's on-disk layout).
///
/// The broker's `CacheManifest` (built in `fbuild-daemon`) maps these to
/// `running-process` `CacheRootKind`s. `CacheRoots` itself is dependency-free so
/// the CLI diagnostic can resolve and print the same paths without pulling in
/// `running-process`.
#[derive(Debug, Clone)]
pub struct CacheRoots {
    pub artifact: PathBuf,
    pub index: PathBuf,
    pub temp: PathBuf,
    pub log: PathBuf,
    pub lock: PathBuf,
    pub runtime: PathBuf,
    pub config: PathBuf,
}

impl CacheRoots {
    /// Resolve fbuild's cache roots.
    ///
    /// `runtime_dir` is the directory holding the relocated `fbuild-daemon`
    /// binary (typically the directory of the current executable); callers pass
    /// it explicitly so this stays a pure function of its inputs and the rest of
    /// this crate's path resolution.
    pub fn discover(runtime_dir: impl Into<PathBuf>) -> Self {
        let cache = crate::get_cache_root();
        let daemon_dir = crate::get_daemon_dir();
        Self {
            index: cache.join("index"),
            artifact: cache,
            temp: crate::get_fbuild_root().join("tmp"),
            log: daemon_dir.clone(),
            lock: daemon_dir,
            runtime: runtime_dir.into(),
            config: crate::get_fbuild_root(),
        }
    }
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

    #[test]
    fn daemon_cache_identity_excludes_backend_version() {
        let identity = DaemonCacheIdentity::discover();
        assert_eq!(identity.trust_domain, LOCAL_TRUST_DOMAIN);
        assert!(matches!(identity.mode, "dev" | "prod"));
        assert!(
            identity.label_value().contains("cache="),
            "identity label must include the cache root key"
        );
        assert!(
            !identity.label_value().contains(env!("CARGO_PKG_VERSION")),
            "backend crate version must not be a cache-owner dimension"
        );
    }
}

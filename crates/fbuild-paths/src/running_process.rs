//! Minimal `running-process` broker adoption seam for fbuild.
//!
//! The current fbuild release still talks to `fbuild-daemon` over its direct
//! loopback HTTP endpoint. This module records the broker-facing service
//! metadata and escape-hatch behavior so future `connect_to_backend` wiring can
//! replace the direct path without scattering policy through CLI/PyO3 callers.

use std::path::{Path, PathBuf};

use fbuild_core::path::NormalizedPath;

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

/// Compatibility version for fbuild-owned shared artifact repository layout.
///
/// Backend package version is deliberately not a cache-owner dimension. This
/// value is the broker-visible compatibility key that future resolver policy
/// should compare before allowing multiple fbuild daemon versions to share the
/// same cache-root identity.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

pub const RUNNING_PROCESS_DISABLE_ENV: &str = "RUNNING_PROCESS_DISABLE";
pub const RUNNING_PROCESS_SERVICE_DEF_DIR_ENV: &str = "RUNNING_PROCESS_SERVICE_DEF_DIR";
pub const FBUILD_RUNNING_PROCESS_BROKER_ENV: &str = "FBUILD_RUNNING_PROCESS_BROKER";
pub const FBUILD_CACHE_DIR_ENV: &str = "FBUILD_CACHE_DIR";
pub const LOCAL_TRUST_DOMAIN: &str = "local-shared";

pub const DAEMON_BINARY_NAME: &str =
    fbuild_core::platform::executable::name("fbuild-daemon", "fbuild-daemon.exe");

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
        Self::from_resolved(
            cache_root,
            crate::is_dev_mode(),
            std::env::var_os(FBUILD_CACHE_DIR_ENV).is_some(),
        )
    }

    fn from_resolved(cache_root: PathBuf, dev_mode: bool, cache_dir_overridden: bool) -> Self {
        Self {
            mode: if dev_mode { "dev" } else { "prod" },
            cache_root_key: stable_path_key(&cache_root),
            cache_dir_source: if cache_dir_overridden {
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
            "mode={};trust={};schema={};cache={}",
            self.mode, self.trust_domain, CACHE_SCHEMA_VERSION, self.cache_root_key
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
    // FastLED/fbuild#911 — delegate the Windows `\` → `/` rewrite to
    // NormalizedPath, the canonical primitive.
    NormalizedPath::from(absolute).display_slash()
}

/// The seven cache roots fbuild records in its broker manifest, resolved from
/// this crate (the single source of truth for fbuild's on-disk layout).
///
/// Ownership contract:
///
/// - `artifact` is the authoritative fbuild global artifact repository root.
///   It comes from `FBUILD_CACHE_DIR` when set, otherwise
///   `~/.fbuild/{dev|prod}/cache`. Package/toolchain/framework archives,
///   installed payloads, `.lnk` blobs, and the disk-cache database live below
///   this root.
/// - `index` is broker metadata for the same artifact repository, not a
///   daemon-version partition.
/// - `temp`, `log`, `lock`, and `config` are fbuild-owned support roots under
///   `~/.fbuild/{dev|prod}`.
/// - `runtime` identifies daemon binary provenance only. Broker runtime
///   version/path changes must not fork `artifact` or `index`.
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
        Self::from_resolved(runtime_dir.into(), cache, crate::get_fbuild_root())
    }

    fn from_resolved(runtime_dir: PathBuf, cache: PathBuf, fbuild_root: PathBuf) -> Self {
        let daemon_dir = fbuild_root.join("daemon");
        Self {
            index: cache.join("index"),
            artifact: cache,
            temp: fbuild_root.join("tmp"),
            log: daemon_dir.clone(),
            lock: daemon_dir,
            runtime: runtime_dir,
            config: fbuild_root,
        }
    }
}

fn platform_service_definition_dir() -> PathBuf {
    if fbuild_core::platform::host::is_windows() {
        return std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|home| PathBuf::from(home).join("AppData").join("Roaming"))
            })
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("running-process")
            .join("services");
    }
    if fbuild_core::platform::host::is_macos() {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| {
                home.join("Library")
                    .join("Application Support")
                    .join("running-process")
                    .join("services")
            })
            .unwrap_or_else(fbuild_owned_service_definition_dir);
    }
    platform_service_definition_dir_from(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        fbuild_owned_service_definition_dir,
    )
}

fn platform_service_definition_dir_from<F>(
    config_home: Option<PathBuf>,
    home: Option<PathBuf>,
    fallback: F,
) -> PathBuf
where
    F: FnOnce() -> PathBuf,
{
    config_home
        .or_else(|| home.map(|home| home.join(".config")))
        .map(|config_home| config_home.join("running-process").join("services"))
        .unwrap_or_else(fallback)
}

fn fbuild_owned_service_definition_dir() -> PathBuf {
    crate::get_cache_root()
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
    fn service_definition_dir_falls_back_to_fbuild_cache_root_without_home() {
        if !fbuild_core::platform::host::is_linux() {
            return;
        }
        let cache_root = crate::temp_subdir(&format!(
            "fbuild-service-def-cache-root-{}",
            std::process::id()
        ));
        let fallback = cache_root.join("running-process").join("services");
        assert_eq!(
            platform_service_definition_dir_from(None, None, || fallback.clone()),
            fallback
        );
    }

    #[test]
    fn service_definition_dir_does_not_resolve_fallback_when_xdg_is_set() {
        let xdg = PathBuf::from("/tmp/xdg");

        assert_eq!(
            platform_service_definition_dir_from(Some(xdg.clone()), None, || {
                panic!("fallback must remain lazy")
            }),
            xdg.join("running-process").join("services")
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
            identity
                .label_value()
                .contains(&format!("schema={CACHE_SCHEMA_VERSION}")),
            "identity label must include the cache schema compatibility version"
        );
        assert!(
            !identity.label_value().contains(env!("CARGO_PKG_VERSION")),
            "backend crate version must not be a cache-owner dimension"
        );
    }

    #[test]
    fn cache_roots_respect_fbuild_cache_dir_as_artifact_owner() {
        let cache_root = crate::temp_subdir(&format!("fbuild-cache-roots-{}", std::process::id()));
        let runtime = PathBuf::from("/opt/fbuild/bin");
        let fbuild_root = PathBuf::from("/home/test")
            .join(crate::FBUILD_DIR_NAME)
            .join("prod");

        let roots = CacheRoots::from_resolved(runtime.clone(), cache_root.clone(), fbuild_root);

        assert_eq!(roots.artifact, cache_root);
        assert_eq!(roots.index, cache_root.join("index"));
        assert_eq!(roots.runtime, runtime);
        assert_ne!(
            roots.artifact, roots.runtime,
            "daemon runtime provenance must not become the artifact repository"
        );
    }

    #[test]
    fn cache_roots_keep_artifacts_stable_across_runtime_dirs() {
        let cache_root =
            crate::temp_subdir(&format!("fbuild-cache-roots-stable-{}", std::process::id()));
        let fbuild_root = PathBuf::from("/home/test")
            .join(crate::FBUILD_DIR_NAME)
            .join("prod");
        let runtime_v1 = PathBuf::from("/opt/fbuild-1/bin");
        let runtime_v2 = PathBuf::from("/opt/fbuild-2/bin");

        let roots_v1 =
            CacheRoots::from_resolved(runtime_v1.clone(), cache_root.clone(), fbuild_root.clone());
        let roots_v2 = CacheRoots::from_resolved(runtime_v2.clone(), cache_root, fbuild_root);

        assert_eq!(roots_v1.artifact, roots_v2.artifact);
        assert_eq!(roots_v1.index, roots_v2.index);
        assert_eq!(roots_v1.temp, roots_v2.temp);
        assert_eq!(roots_v1.log, roots_v2.log);
        assert_eq!(roots_v1.lock, roots_v2.lock);
        assert_eq!(roots_v1.config, roots_v2.config);
        assert_ne!(
            roots_v1.runtime, roots_v2.runtime,
            "runtime changes are daemon provenance, not cache ownership"
        );
    }

    #[test]
    fn dev_mode_default_cache_roots_stay_stable_across_runtime_dirs() {
        let runtime_v1 = PathBuf::from("/opt/fbuild-dev-1/bin");
        let runtime_v2 = PathBuf::from("/opt/fbuild-dev-2/bin");
        let fbuild_root = PathBuf::from("/home/test")
            .join(crate::FBUILD_DIR_NAME)
            .join("dev");
        let cache_root = fbuild_root.join("cache");

        let identity = DaemonCacheIdentity::from_resolved(cache_root.clone(), true, false);
        let roots_v1 =
            CacheRoots::from_resolved(runtime_v1.clone(), cache_root.clone(), fbuild_root.clone());
        let roots_v2 = CacheRoots::from_resolved(runtime_v2.clone(), cache_root, fbuild_root);

        assert_eq!(identity.mode, "dev");
        assert_eq!(identity.cache_dir_source, "default");
        assert!(
            identity
                .cache_root
                .ends_with(Path::new(crate::FBUILD_DIR_NAME).join("dev").join("cache")),
            "dev identity must own the default dev cache root, got {}",
            identity.cache_root.display()
        );
        assert_eq!(roots_v1.artifact, identity.cache_root);
        assert_eq!(roots_v1.artifact, roots_v2.artifact);
        assert_eq!(roots_v1.index, roots_v2.index);
        assert_ne!(
            roots_v1.runtime, roots_v2.runtime,
            "runtime changes are daemon provenance, not cache ownership"
        );
    }
}

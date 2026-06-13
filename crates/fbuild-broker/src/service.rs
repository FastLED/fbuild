//! fbuild `ServiceDefinition` + `CacheManifest` construction for the v1 broker.
//!
//! These are the two registration messages the broker needs to find/spawn
//! fbuild and let peers discover its cache. Built with the frozen
//! `ServiceDefinitionBuilder` / `CacheManifestBuilder` (zackees/running-process
//! #433) rather than hand-written textproto.
//!
//! - **Default isolation** is `SHARED_BROKER` — a per-user local fbuild daemon.
//! - **CI isolation** is `EXPLICIT_INSTANCE "ci-trusted"` — CI jobs that
//!   intentionally isolate trust groups (see the inventory's CI-trust-grouping
//!   record).

use std::path::{Path, PathBuf};

use running_process::broker::builders::{CacheManifestBuilder, ServiceDefinitionBuilder};
use running_process::broker::protocol::{CacheManifest, CacheRootKind, ServiceDefinition};

/// fbuild's broker service name (matches the `.servicedef` and Hello).
pub const SERVICE_NAME: &str = "fbuild";

/// The trust-group label CI uses for `EXPLICIT_INSTANCE` isolation.
pub const CI_TRUSTED_INSTANCE: &str = "ci-trusted";

/// Minimum acceptable fbuild backend version the broker will negotiate.
pub const MIN_VERSION: &str = "1.0.0";

/// Errors building or installing the fbuild service definition / manifest.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// The `ServiceDefinition` failed validation or could not be installed.
    #[error("service definition: {0}")]
    ServiceDefinition(
        #[from] running_process::broker::server::service_def_loader::ServiceDefinitionError,
    ),
    /// The `CacheManifest` failed to build or publish.
    #[error("cache manifest: {0}")]
    Manifest(#[from] running_process::broker::manifest::ManifestError),
}

/// The seven cache roots fbuild records in its manifest, resolved from
/// `fbuild-paths` (the single source of truth for fbuild's on-disk layout).
///
/// Mapping to broker [`CacheRootKind`]s:
///
/// | fbuild root | path source                              | kind          |
/// |-------------|------------------------------------------|---------------|
/// | artifact    | `get_cache_root()`                       | `CacheData`   |
/// | index       | `<cache>/index`                          | `CacheIndex`  |
/// | temp        | `<fbuild_root>/tmp`                       | `CacheTmp`    |
/// | log         | `get_daemon_dir()` (daemon.log lives here)| `CacheLogs`  |
/// | lock        | `get_daemon_dir()` (pid/port/lock files) | `CacheLocks`  |
/// | runtime     | daemon binary directory                  | `CacheRuntime`|
/// | config      | `get_fbuild_root()`                       | `CacheConfig` |
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
    /// Resolve fbuild's cache roots from `fbuild-paths`.
    ///
    /// `runtime_dir` is the directory holding the relocated `fbuild-daemon`
    /// binary (typically the directory of the current executable); callers pass
    /// it explicitly so this stays a pure function of its inputs and
    /// `fbuild-paths`.
    pub fn discover(runtime_dir: impl Into<PathBuf>) -> Self {
        let cache = fbuild_paths::get_cache_root();
        let daemon_dir = fbuild_paths::get_daemon_dir();
        Self {
            index: cache.join("index"),
            artifact: cache,
            temp: fbuild_paths::get_fbuild_root().join("tmp"),
            log: daemon_dir.clone(),
            lock: daemon_dir,
            runtime: runtime_dir.into(),
            config: fbuild_paths::get_fbuild_root(),
        }
    }

    fn entries(&self) -> [(CacheRootKind, &Path); 7] {
        [
            (CacheRootKind::CacheData, self.artifact.as_path()),
            (CacheRootKind::CacheIndex, self.index.as_path()),
            (CacheRootKind::CacheTmp, self.temp.as_path()),
            (CacheRootKind::CacheLogs, self.log.as_path()),
            (CacheRootKind::CacheLocks, self.lock.as_path()),
            (CacheRootKind::CacheRuntime, self.runtime.as_path()),
            (CacheRootKind::CacheConfig, self.config.as_path()),
        ]
    }
}

/// Build the validated `SHARED_BROKER` (per-user local) fbuild service
/// definition. `daemon_binary` must be an absolute path.
pub fn fbuild_service_definition(
    daemon_binary: impl AsRef<Path>,
) -> Result<ServiceDefinition, ServiceError> {
    Ok(shared_broker_builder(daemon_binary).build()?)
}

/// Build the validated `EXPLICIT_INSTANCE "ci-trusted"` fbuild service
/// definition for CI trust-grouped jobs. `daemon_binary` must be absolute.
pub fn fbuild_ci_service_definition(
    daemon_binary: impl AsRef<Path>,
) -> Result<ServiceDefinition, ServiceError> {
    Ok(ci_builder(daemon_binary).build()?)
}

/// Install the per-user local (`SHARED_BROKER`) service definition into the
/// platform service-definition directory, returning the written path.
pub fn install_fbuild_service_definition(
    daemon_binary: impl AsRef<Path>,
) -> Result<PathBuf, ServiceError> {
    Ok(shared_broker_builder(daemon_binary).install()?)
}

/// Install a service definition into an explicit root (tests / custom layouts).
pub fn install_fbuild_service_definition_in(
    daemon_binary: impl AsRef<Path>,
    root: &Path,
) -> Result<PathBuf, ServiceError> {
    Ok(shared_broker_builder(daemon_binary).install_in(root)?)
}

fn shared_broker_builder(daemon_binary: impl AsRef<Path>) -> ServiceDefinitionBuilder {
    ServiceDefinitionBuilder::shared_broker(
        SERVICE_NAME,
        daemon_binary.as_ref().display().to_string(),
    )
    .min_version(MIN_VERSION)
    .label("consumer", "fbuild")
    .label("repo", "FastLED/fbuild")
    .label("tracker", "FastLED/fbuild#510")
    .label("running-process-tracker", "zackees/running-process#437")
}

fn ci_builder(daemon_binary: impl AsRef<Path>) -> ServiceDefinitionBuilder {
    ServiceDefinitionBuilder::explicit_instance(
        SERVICE_NAME,
        daemon_binary.as_ref().display().to_string(),
        CI_TRUSTED_INSTANCE,
    )
    .min_version(MIN_VERSION)
    .label("consumer", "fbuild")
    .label("trust-domain", CI_TRUSTED_INSTANCE)
    .label("repo", "FastLED/fbuild")
}

/// Build the fbuild `CacheManifest` recording all seven cache roots.
pub fn fbuild_cache_manifest(
    service_version: impl Into<String>,
    roots: &CacheRoots,
) -> Result<CacheManifest, ServiceError> {
    Ok(manifest_builder(service_version, roots).build()?)
}

/// Publish the fbuild `CacheManifest` into the central registry.
pub fn publish_fbuild_cache_manifest(
    service_version: impl Into<String>,
    roots: &CacheRoots,
) -> Result<PathBuf, ServiceError> {
    Ok(manifest_builder(service_version, roots).publish()?)
}

/// Publish the manifest into an explicit registry root (tests).
pub fn publish_fbuild_cache_manifest_in(
    service_version: impl Into<String>,
    roots: &CacheRoots,
    registry_dir: &Path,
) -> Result<PathBuf, ServiceError> {
    Ok(manifest_builder(service_version, roots).publish_in(registry_dir)?)
}

fn manifest_builder(
    service_version: impl Into<String>,
    roots: &CacheRoots,
) -> CacheManifestBuilder {
    let mut builder = CacheManifestBuilder::new(SERVICE_NAME, service_version).broker_instance(
        // SHARED_BROKER local daemons advertise the "shared" instance.
        "shared",
    );
    for (kind, path) in roots.entries() {
        builder = builder.root(kind, path.display().to_string());
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs_daemon() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\opt\fbuild\bin\fbuild-daemon.exe")
        } else {
            PathBuf::from("/opt/fbuild/bin/fbuild-daemon")
        }
    }

    #[test]
    fn shared_broker_service_definition_validates() {
        let def = fbuild_service_definition(abs_daemon()).expect("build shared definition");
        assert_eq!(def.service_name, "fbuild");
        assert_eq!(def.min_version, MIN_VERSION);
        // SHARED_BROKER isolation discriminant.
        assert_eq!(
            def.isolation,
            running_process::broker::protocol::BrokerIsolation::SharedBroker as i32
        );
        assert_eq!(
            def.labels.get("consumer").map(String::as_str),
            Some("fbuild")
        );
    }

    #[test]
    fn ci_explicit_instance_service_definition_validates() {
        let def = fbuild_ci_service_definition(abs_daemon()).expect("build ci definition");
        assert_eq!(def.service_name, "fbuild");
        assert_eq!(def.explicit_instance, CI_TRUSTED_INSTANCE);
        assert_eq!(
            def.isolation,
            running_process::broker::protocol::BrokerIsolation::ExplicitInstance as i32
        );
        assert_eq!(
            def.labels.get("trust-domain").map(String::as_str),
            Some(CI_TRUSTED_INSTANCE)
        );
    }

    #[test]
    fn relative_binary_path_is_rejected() {
        // The broker rejects a relative binary_path on build.
        let err = fbuild_service_definition(PathBuf::from("fbuild-daemon"));
        assert!(err.is_err(), "relative binary path must be rejected");
    }

    #[test]
    fn install_and_validate_roundtrip_in_temp_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let written =
            install_fbuild_service_definition_in(abs_daemon(), tmp.path()).expect("install");
        assert!(written.exists(), "servicedef must be written to disk");
    }

    #[test]
    fn cache_manifest_records_all_seven_roots() {
        let runtime = if cfg!(windows) {
            PathBuf::from(r"C:\opt\fbuild\bin")
        } else {
            PathBuf::from("/opt/fbuild/bin")
        };
        let roots = CacheRoots::discover(runtime);
        let manifest = fbuild_cache_manifest("2.2.27", &roots).expect("build manifest");
        assert_eq!(manifest.service_name, "fbuild");
        assert_eq!(manifest.roots.len(), 7, "all 7 cache roots must be present");

        // Every required kind must appear exactly once.
        let kinds: Vec<i32> = manifest.roots.iter().map(|r| r.kind).collect();
        for kind in [
            CacheRootKind::CacheData,
            CacheRootKind::CacheIndex,
            CacheRootKind::CacheTmp,
            CacheRootKind::CacheLogs,
            CacheRootKind::CacheLocks,
            CacheRootKind::CacheRuntime,
            CacheRootKind::CacheConfig,
        ] {
            assert!(
                kinds.contains(&(kind as i32)),
                "manifest missing cache root kind {kind:?}"
            );
        }
    }

    #[test]
    fn cache_manifest_publish_roundtrip_in_temp_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runtime = tmp.path().join("bin");
        let roots = CacheRoots::discover(runtime);
        let written =
            publish_fbuild_cache_manifest_in("2.2.27", &roots, tmp.path()).expect("publish");
        assert!(written.exists(), "manifest must be written to disk");
    }
}

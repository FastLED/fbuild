//! Provision an environment's packages without compiling — the engine behind
//! `fbuild install` (FastLED/fbuild#1433).
//!
//! Each platform lists what its build downloads through
//! [`crate::PlatformSupport::provision`]; [`provision_package`] turns one
//! [`Package`] into a report row, so every platform reports presence, fetches,
//! durations and sizes the same way. [`ProvisionMode::Check`] and
//! [`ProvisionMode::DryRun`] never call `ensure_installed`, so they never touch
//! the network.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use fbuild_config::BoardConfig;
use fbuild_packages::Package;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// What provisioning is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionMode {
    /// Fetch anything missing.
    Install,
    /// Report what is missing without fetching; callers exit non-zero when
    /// something would need fetching.
    Check,
    /// Report the resolved set without fetching.
    DryRun,
}

impl ProvisionMode {
    /// Whether this mode may download and install.
    pub fn fetches(self) -> bool {
        matches!(self, Self::Install)
    }
}

/// Outcome for one package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvisionStatus {
    /// Already installed before this run.
    Present,
    /// Installed by this run.
    Fetched,
    /// Missing; a check or dry run did not fetch it.
    WouldFetch,
    /// Fetching failed.
    Failed,
}

impl ProvisionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Fetched => "fetched",
            Self::WouldFetch => "would-fetch",
            Self::Failed => "failed",
        }
    }
}

/// What role a package plays in the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageKind {
    Platform,
    Toolchain,
    Framework,
    SdkLibs,
    Tool,
    Library,
}

impl PackageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Toolchain => "toolchain",
            Self::Framework => "framework",
            Self::SdkLibs => "sdk-libs",
            Self::Tool => "tool",
            Self::Library => "library",
        }
    }
}

/// One row of a provisioning report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProvisionedPackage {
    pub kind: PackageKind,
    pub name: String,
    pub version: String,
    pub url: String,
    pub sha256: Option<String>,
    pub status: ProvisionStatus,
    /// Installed size from the package cache index, when it has a row.
    pub bytes: Option<u64>,
    pub duration_ms: u64,
    pub install_path: Option<String>,
    pub error: Option<String>,
}

impl ProvisionedPackage {
    /// A row for a package whose presence was decided outside [`Package`]
    /// (SDK libs, standalone tools, libraries).
    pub fn new(kind: PackageKind, name: impl Into<String>, status: ProvisionStatus) -> Self {
        Self {
            kind,
            name: name.into(),
            version: String::new(),
            url: String::new(),
            sha256: None,
            status,
            bytes: None,
            duration_ms: 0,
            install_path: None,
            error: None,
        }
    }
}

/// Everything a platform needs to resolve its packages for one env.
pub struct ProvisionInputs<'a> {
    pub project_dir: &'a Path,
    pub env_name: &'a str,
    pub env_config: &'a HashMap<String, String>,
    pub board: &'a BoardConfig,
}

/// Provision one [`Package`] and describe the outcome.
pub async fn provision_package(
    kind: PackageKind,
    package: &dyn Package,
    mode: ProvisionMode,
) -> ProvisionedPackage {
    let started = Instant::now();
    let (status, error) = if package.is_installed() {
        (ProvisionStatus::Present, None)
    } else if !mode.fetches() {
        (ProvisionStatus::WouldFetch, None)
    } else {
        match package.ensure_installed().await {
            Ok(_) => (ProvisionStatus::Fetched, None),
            Err(error) => (ProvisionStatus::Failed, Some(error.to_string())),
        }
    };
    let info = package.get_info();
    let installed = matches!(status, ProvisionStatus::Present | ProvisionStatus::Fetched);
    ProvisionedPackage {
        kind,
        name: info.name,
        version: info.version,
        url: info.url,
        sha256: info.checksum,
        status,
        bytes: if installed {
            info.installed_bytes
        } else {
            None
        },
        duration_ms: started.elapsed().as_millis() as u64,
        install_path: Some(info.install_path.display().to_string()),
        error,
    }
}

/// Provision `lib_deps` into `libs_dir` — the directory the build downloads
/// them into — without compiling. A check or dry run reports direct
/// dependencies only: transitive ones are only known once the direct ones are
/// downloaded.
pub async fn provision_lib_deps(
    project_dir: &Path,
    lib_deps: &[String],
    lib_ignore: &[String],
    libs_dir: &Path,
    mode: ProvisionMode,
) -> Vec<ProvisionedPackage> {
    use fbuild_packages::library::{library_downloader, library_manager};

    let specs = library_manager::parse_lib_specs(lib_deps, lib_ignore);
    let started = Instant::now();
    let present_before: Vec<bool> = specs
        .iter()
        .map(|spec| spec.local_path.is_some() || library_downloader::is_downloaded(spec, libs_dir))
        .collect();
    let mut rows: Vec<ProvisionedPackage> = specs
        .iter()
        .zip(&present_before)
        .map(|(spec, present)| {
            let status = if *present {
                ProvisionStatus::Present
            } else {
                ProvisionStatus::WouldFetch
            };
            library_row(spec, libs_dir, status)
        })
        .collect();
    if !mode.fetches() || present_before.iter().all(|present| *present) {
        return rows;
    }

    match library_manager::download_libraries(lib_deps, lib_ignore, project_dir, libs_dir).await {
        Ok(installed) => {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            for row in rows
                .iter_mut()
                .filter(|row| row.status == ProvisionStatus::WouldFetch)
            {
                row.status = ProvisionStatus::Fetched;
                row.duration_ms = elapsed_ms;
            }
            for library in installed {
                let path = library.lib_dir.display().to_string();
                if rows
                    .iter()
                    .all(|row| row.install_path.as_deref() != Some(path.as_str()))
                {
                    rows.push(ProvisionedPackage {
                        install_path: Some(path),
                        duration_ms: elapsed_ms,
                        ..ProvisionedPackage::new(
                            PackageKind::Library,
                            library.name,
                            ProvisionStatus::Fetched,
                        )
                    });
                }
            }
        }
        Err(error) => {
            for row in rows
                .iter_mut()
                .filter(|row| row.status == ProvisionStatus::WouldFetch)
            {
                row.status = ProvisionStatus::Failed;
                row.error = Some(error.to_string());
            }
        }
    }
    rows
}

fn library_row(
    spec: &fbuild_packages::library::library_spec::LibrarySpec,
    libs_dir: &Path,
    status: ProvisionStatus,
) -> ProvisionedPackage {
    let name = if spec.owner.is_empty() {
        spec.name.clone()
    } else {
        format!("{}/{}", spec.owner, spec.name)
    };
    let (url, install_path) = match (&spec.local_path, &spec.github_url) {
        (Some(local), _) => (
            format!("file://{}", local.display()),
            local.display().to_string(),
        ),
        (None, Some(github)) => (
            github.clone(),
            libs_dir.join(spec.sanitized_name()).display().to_string(),
        ),
        (None, None) => (
            format!("registry:{name}"),
            libs_dir.join(spec.sanitized_name()).display().to_string(),
        ),
    };
    ProvisionedPackage {
        version: spec.version.clone().unwrap_or_default(),
        url,
        install_path: Some(install_path),
        ..ProvisionedPackage::new(PackageKind::Library, name, status)
    }
}

/// An env's provisioning report.
#[derive(Debug, Clone, Serialize)]
pub struct ProvisionReport {
    pub env: String,
    pub platform: String,
    pub packages: Vec<ProvisionedPackage>,
}

impl ProvisionReport {
    /// True when a check or dry run found something missing.
    pub fn needs_fetch(&self) -> bool {
        self.packages
            .iter()
            .any(|p| p.status == ProvisionStatus::WouldFetch)
    }

    /// True when any package failed to install.
    pub fn failed(&self) -> bool {
        self.packages
            .iter()
            .any(|p| p.status == ProvisionStatus::Failed)
    }
}

/// Content hash of a package set: sha256 over the sorted
/// `(kind, name, version, url, sha256)` tuples. Status, sizes and paths are
/// left out, so a cold and a warm run of the same set hash the same — it is a
/// cache key for "which packages", not "what state are they in".
pub fn packages_hash<'a>(packages: impl IntoIterator<Item = &'a ProvisionedPackage>) -> String {
    let mut tuples: Vec<_> = packages
        .into_iter()
        .map(|p| {
            (
                p.kind.as_str(),
                p.name.as_str(),
                p.version.as_str(),
                p.url.as_str(),
                p.sha256.as_deref().unwrap_or(""),
            )
        })
        .collect();
    tuples.sort();
    tuples.dedup();
    let mut hasher = Sha256::new();
    for (kind, name, version, url, sha256) in tuples {
        for field in [kind, name, version, url, sha256] {
            hasher.update(field.as_bytes());
            hasher.update([0]);
        }
        hasher.update([0xff]);
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_packages::PackageInfo;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FakePackage {
        installed: AtomicBool,
        install_fails: bool,
    }

    impl FakePackage {
        fn new(installed: bool, install_fails: bool) -> Self {
            Self {
                installed: AtomicBool::new(installed),
                install_fails,
            }
        }
    }

    #[async_trait::async_trait]
    impl Package for FakePackage {
        async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
            if self.install_fails {
                return Err(fbuild_core::FbuildError::PackageError("404".into()));
            }
            self.installed.store(true, Ordering::SeqCst);
            Ok(PathBuf::from("/cache/fake"))
        }

        fn is_installed(&self) -> bool {
            self.installed.load(Ordering::SeqCst)
        }

        fn get_info(&self) -> PackageInfo {
            PackageInfo {
                name: "toolchain-fake".into(),
                version: "1.2.3".into(),
                url: "https://example.com/fake.tar.gz".into(),
                install_path: PathBuf::from("/cache/fake"),
                checksum: Some("abc123".into()),
                installed_bytes: Some(4096),
            }
        }
    }

    #[tokio::test]
    async fn present_package_is_reported_present_in_every_mode() {
        for mode in [
            ProvisionMode::Install,
            ProvisionMode::Check,
            ProvisionMode::DryRun,
        ] {
            let row =
                provision_package(PackageKind::Toolchain, &FakePackage::new(true, true), mode)
                    .await;
            assert_eq!(row.status, ProvisionStatus::Present, "{mode:?}");
            assert_eq!(row.bytes, Some(4096));
            assert_eq!(row.sha256.as_deref(), Some("abc123"));
        }
    }

    #[tokio::test]
    async fn missing_package_is_fetched_only_by_install() {
        let fake = FakePackage::new(false, false);
        let row = provision_package(PackageKind::Toolchain, &fake, ProvisionMode::Check).await;
        assert_eq!(row.status, ProvisionStatus::WouldFetch);
        assert_eq!(row.bytes, None);
        assert!(!fake.is_installed(), "a check must not install");

        let row = provision_package(PackageKind::Toolchain, &fake, ProvisionMode::DryRun).await;
        assert_eq!(row.status, ProvisionStatus::WouldFetch);
        assert!(!fake.is_installed(), "a dry run must not install");

        let row = provision_package(PackageKind::Toolchain, &fake, ProvisionMode::Install).await;
        assert_eq!(row.status, ProvisionStatus::Fetched);
        assert!(fake.is_installed());
    }

    #[tokio::test]
    async fn failed_install_carries_the_error() {
        let row = provision_package(
            PackageKind::Framework,
            &FakePackage::new(false, true),
            ProvisionMode::Install,
        )
        .await;
        assert_eq!(row.status, ProvisionStatus::Failed);
        assert!(row.error.as_deref().unwrap_or("").contains("404"));
    }

    fn row(name: &str, status: ProvisionStatus, bytes: Option<u64>) -> ProvisionedPackage {
        ProvisionedPackage {
            version: "1".into(),
            url: format!("https://example.com/{name}"),
            bytes,
            ..ProvisionedPackage::new(PackageKind::Toolchain, name, status)
        }
    }

    #[test]
    fn packages_hash_ignores_order_status_and_size() {
        let cold = [
            row("a", ProvisionStatus::WouldFetch, None),
            row("b", ProvisionStatus::WouldFetch, None),
        ];
        let warm = [
            row("b", ProvisionStatus::Present, Some(10)),
            row("a", ProvisionStatus::Fetched, Some(20)),
        ];
        assert_eq!(packages_hash(&cold), packages_hash(&warm));
    }

    #[test]
    fn packages_hash_changes_with_the_package_set() {
        let one = [row("a", ProvisionStatus::Present, None)];
        let two = [
            row("a", ProvisionStatus::Present, None),
            row("b", ProvisionStatus::Present, None),
        ];
        assert_ne!(packages_hash(&one), packages_hash(&two));
    }

    #[test]
    fn report_flags_needs_fetch_and_failure() {
        let report = ProvisionReport {
            env: "uno".into(),
            platform: "AtmelAvr".into(),
            packages: vec![
                row("a", ProvisionStatus::Present, None),
                row("b", ProvisionStatus::WouldFetch, None),
            ],
        };
        assert!(report.needs_fetch());
        assert!(!report.failed());
    }

    #[test]
    fn statuses_serialize_kebab_case() {
        assert_eq!(
            serde_json::to_string(&ProvisionStatus::WouldFetch).unwrap(),
            "\"would-fetch\""
        );
        assert_eq!(
            serde_json::to_string(&PackageKind::SdkLibs).unwrap(),
            "\"sdk-libs\""
        );
    }
}

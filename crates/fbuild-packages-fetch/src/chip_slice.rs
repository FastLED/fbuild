//! Per-chip archive slices for packages installed through [`PackageBase`]
//! (FastLED/fbuild#1944).
//!
//! The ESP32 toolchain ships one 660 MB tree for every Xtensa chip, but the
//! per-chip SDK libraries (`lib/esp32`, `lib/esp32s2`, `lib/esp32s3`, ...) are
//! the removable bulk: the compiler, linker and top-level multilibs are chip
//! shared. `ci/slice_toolchain.py` publishes one archive per chip, and a
//! package that knows its chip prefers that archive when the mirror lists it.
//!
//! Two rules make the client side safe:
//!
//! * A slice installs into its own chip-scoped cache directory. A slice holds
//!   ONE chip's libraries, so putting it in the shared directory would let a
//!   build for a different chip find a tree that looks installed and fail at
//!   link. A whole archive keeps the shared directory, which serves every
//!   chip, and is preferred when it already exists (no re-download for hosts
//!   that installed before slicing existed).
//! * A slice that IS listed is authoritative: a corrupt one fails the install
//!   instead of quietly downloading the full archive, so a bad publish is
//!   visible rather than merely slow. Only "no slice published for this chip"
//!   falls back.
use std::path::Path;

use fbuild_core::FbuildError;
use fbuild_core::path::NormalizedPath;

use crate::install_status::{self, InstallPhase, InstallRole};
use crate::{PackageBase, downloader, variants};

impl PackageBase {
    /// The chip-scoped install path, when this package prefers a slice.
    pub fn chip_install_path(&self) -> Option<NormalizedPath> {
        let chip = self.slice_chip.as_deref()?;
        let shared = self.install_path();
        let name = shared.file_name()?.to_string_lossy().into_owned();
        Some(NormalizedPath::new(
            shared.with_file_name(format!("{name}+{chip}")),
        ))
    }

    /// Prefer a published per-chip slice for `chip`. The token is normalized
    /// exactly like the manifest's slice keys.
    pub fn set_slice_variant(&mut self, chip: &str) {
        self.slice_chip = Some(variants::chip_name(chip));
    }

    /// Builder form of [`Self::set_slice_variant`].
    pub fn with_slice_variant(mut self, chip: &str) -> Self {
        self.set_slice_variant(chip);
        self
    }

    /// The install directory actually on disk: the shared tree when it exists
    /// (a whole archive serves every chip), else this chip's slice.
    pub fn installed_path(&self) -> NormalizedPath {
        self.installed_dirs()
            .into_iter()
            .find(|path| Self::is_complete_install(path.as_path()))
            .unwrap_or_else(|| NormalizedPath::new(self.install_path()))
    }

    /// Shared install first, then this chip's slice.
    pub(crate) fn installed_dirs(&self) -> Vec<NormalizedPath> {
        let mut dirs = vec![NormalizedPath::new(self.install_path())];
        dirs.extend(self.chip_install_path());
        dirs
    }

    pub(crate) fn is_complete_install(path: &Path) -> bool {
        path.is_dir() && crate::disk_cache::paths::install_complete_sentinel(path).exists()
    }

    /// The cache version token, chip-scoped when a slice may be installed, so
    /// the DiskCache index keeps one row per chip.
    pub(crate) fn effective_version(&self) -> String {
        match &self.slice_chip {
            Some(chip) => format!("{}+{chip}", self.version),
            None => self.version.clone(),
        }
    }

    /// The archive to install: a published slice for this chip when the mirror
    /// lists one, else the artifact's own URL.
    pub(crate) async fn acquire_archive(&self, dir: &Path) -> fbuild_core::Result<NormalizedPath> {
        match self.published_slice().await {
            Some(slice) => self.download_slice(&slice, dir).await,
            None => {
                tracing::info!("downloading {} v{}", self.name, self.version);
                let name = self.name.clone();
                let version = self.version.clone();
                downloader::download_file_with_progress(
                    &self.url,
                    dir,
                    &mut |progress: &downloader::DownloadProgress| {
                        install_status::publish_install_status(install_status::status(
                            &name,
                            Some(&version),
                            InstallPhase::Downloading,
                            InstallRole::Installer,
                            progress.format_message(),
                            None::<String>,
                        ));
                    },
                )
                .await
                .map(NormalizedPath::new)
            }
        }
    }

    async fn published_slice(&self) -> Option<variants::SliceFetch> {
        #[cfg(test)]
        if let Some(slice) = &self.forced_slice {
            return Some(slice.clone());
        }
        let chip = self.slice_chip.as_deref()?;
        variants::published_slice(&self.name, &self.version, &self.url, chip).await
    }

    async fn download_slice(
        &self,
        slice: &variants::SliceFetch,
        dir: &Path,
    ) -> fbuild_core::Result<NormalizedPath> {
        let chip = self.slice_chip.as_deref().unwrap_or("?");
        tracing::info!(
            "downloading {} v{} {} chip slice ({} MB)",
            self.name,
            self.version,
            chip,
            slice.size / 1_000_000
        );
        let body = crate::http::client()
            .get(&slice.url)
            .send()
            .await
            .map_err(|e| FbuildError::PackageError(e.to_string()))?
            .error_for_status()
            .map_err(|e| FbuildError::PackageError(e.to_string()))?
            .bytes()
            .await
            .map_err(|e| FbuildError::PackageError(e.to_string()))?;
        if body.len() as u64 != slice.size {
            return Err(FbuildError::PackageError(format!(
                "slice size mismatch for {}: listed {} bytes, downloaded {}",
                slice.url,
                slice.size,
                body.len()
            )));
        }
        // `.tar.zst` so the extractor selects the zstd reader.
        let target = NormalizedPath::new(dir).join("slice.tar.zst");
        std::fs::write(&target, &body)?;
        downloader::verify_checksum(&target, &slice.sha256)?;
        Ok(target)
    }
}

//! Short install paths for packages with deep Windows archive layouts.

use std::path::Path;

use fbuild_core::path::NormalizedPath;
use sha2::{Digest, Sha256};

use crate::{PackageBase, disk_cache::Kind, extractor};

impl PackageBase {
    /// Opt in when a package supports installation without its archive wrapper.
    pub fn with_compact_windows_install(mut self) -> Self {
        self.compact_windows_install = true;
        self
    }

    fn uses_compact_windows_install(&self) -> bool {
        self.compact_windows_install
            && fbuild_paths::long_paths::current_resolution().is_some_and(|resolution| {
                resolution.observation.requires_fallback()
                    || resolution.cache_dir_source == "retained-windows-short-path-fallback"
            })
    }

    pub(crate) fn compact_install_path(&self, chip: Option<&str>) -> Option<NormalizedPath> {
        if !self.uses_compact_windows_install() {
            return None;
        }
        let kind = Kind::from(self.cache_subdir);
        let mut hash = Sha256::new();
        for part in [
            kind.as_str(),
            &self.cache_key,
            &self.version,
            chip.unwrap_or(""),
        ] {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
        let hex = format!("{:x}", hash.finalize());
        Some(NormalizedPath::new(self.cache.cache_root()).join(&hex[..16]))
    }

    pub(crate) fn install_staging_path(&self, target: &Path) -> NormalizedPath {
        let name = target.file_name().unwrap_or_default().to_string_lossy();
        if self.uses_compact_windows_install() {
            NormalizedPath::new(target.with_file_name(format!("s{name}")))
        } else {
            NormalizedPath::new(target.with_file_name(format!("{name}_staging")))
        }
    }

    pub(crate) fn extract_install_archive(
        &self,
        archive: &Path,
        staging: &Path,
    ) -> fbuild_core::Result<()> {
        if !self.uses_compact_windows_install() {
            return extractor::extract(archive, staging);
        }
        unpack_without_wrapper(archive, staging)?;
        extractor::validate_extraction(staging)
    }
}

fn unpack_without_wrapper(archive: &Path, staging: &Path) -> fbuild_core::Result<()> {
    let unpacked = tempfile::Builder::new().prefix("u").tempdir_in(staging)?;
    extractor::unpack(archive, unpacked.path())?;
    let entries = std::fs::read_dir(unpacked.path())?.collect::<std::io::Result<Vec<_>>>()?;
    let payload = match entries.as_slice() {
        [entry]
            if entry.file_type()?.is_dir()
                && (entry.path().join("bin").is_dir() || entry.path().join("cores").is_dir()) =>
        {
            entry.path()
        }
        _ => unpacked.path().to_path_buf(),
    };
    for entry in std::fs::read_dir(payload)? {
        let entry = entry?;
        fbuild_core::platform::fs::rename_path(&entry.path(), &staging.join(entry.file_name()))?;
    }
    unpacked.close()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn compact_install_strips_the_archive_wrapper_and_preserves_inner_paths() {
        let fixture = tempfile::tempdir().unwrap();
        let archive = fixture.path().join("toolchain.zip");
        let staging = fixture.path().join("staging");
        std::fs::create_dir(&staging).unwrap();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        for entry in ["toolchain/bin/cc.exe", "toolchain/lib/runtime.a"] {
            zip.start_file(entry, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(entry.as_bytes()).unwrap();
        }
        zip.finish().unwrap();

        unpack_without_wrapper(&archive, &staging).unwrap();

        assert_eq!(
            std::fs::read(staging.join("bin/cc.exe")).unwrap(),
            b"toolchain/bin/cc.exe"
        );
        assert!(staging.join("lib/runtime.a").is_file());
        assert!(!staging.join("toolchain").exists());
    }
}

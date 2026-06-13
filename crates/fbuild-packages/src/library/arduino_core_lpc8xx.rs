//! Arduino core framework for NXP LPC8xx (`zackees/ArduinoCore-LPC8xx`).
//!
//! Downloads and vendors the real Arduino-compatible core
//! ([zackees/ArduinoCore-LPC8xx](https://github.com/zackees/ArduinoCore-LPC8xx))
//! that supersedes the embedded `arduino_stub/` shim the nxplpc orchestrator
//! previously materialised (FastLED/fbuild#479, #487). The package ships the
//! framework `main()`, startup, wiring/HardwareSerial/SPI implementations
//! (`cores/lpc8xx/`), per-board variants (`variants/<variant>/`), and the GCC
//! linker scripts (`linker_scripts/gcc/`).
//!
//! Pinned to a specific commit so the build is reproducible; the GitHub
//! archive tarball is content-addressed by sha256.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo};

/// Pinned upstream commit. Bump alongside `URL` + `CHECKSUM`.
/// Pinned to the `_init`/`_fini` stub fix (zackees/ArduinoCore-LPC8xx#23) so
/// `-nostartfiles` links resolve; fold back to a merged `main` SHA later.
const ACLPC_COMMIT: &str = "6031232432601d6174a1b8fc7dd361cfb1f9fbea";
const ACLPC_VERSION: &str = "0.1.0+g6031232";
const ACLPC_URL: &str =
    "https://github.com/zackees/ArduinoCore-LPC8xx/archive/6031232432601d6174a1b8fc7dd361cfb1f9fbea.tar.gz";
const ACLPC_CHECKSUM: &str = "7f4ec8fced3b023ae49818b668e387d3056d05e4e170b190a5b4b1a62805b406";

/// Arduino LPC8xx core framework manager.
pub struct ArduinoCoreLpc8xx {
    base: PackageBase,
}

impl ArduinoCoreLpc8xx {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "framework-arduino-lpc8xx",
                ACLPC_VERSION,
                ACLPC_URL,
                ACLPC_URL,
                Some(ACLPC_CHECKSUM),
                CacheSubdir::Platforms,
                project_dir,
            ),
        }
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        Self {
            base: PackageBase::with_cache_root(
                "framework-arduino-lpc8xx",
                ACLPC_VERSION,
                ACLPC_URL,
                ACLPC_URL,
                Some(ACLPC_CHECKSUM),
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
        }
    }

    /// The pinned upstream commit SHA.
    pub fn commit() -> &'static str {
        ACLPC_COMMIT
    }

    /// Validate the extracted package has the expected core layout.
    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        for rel in ["cores/lpc8xx/main.cpp", "cores/lpc8xx/startup_lpc8xx.c"] {
            if !install_dir.join(rel).exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "ArduinoCore-LPC8xx missing {} (in {})",
                    rel,
                    install_dir.display()
                )));
            }
        }
        Ok(())
    }

    /// Repository root of the vendored package. Used as the linker `-L`
    /// search root so a linker script's relative `INCLUDE` directive
    /// (e.g. `INCLUDE linker_scripts/gcc/lpc8xx_common.ld`) resolves.
    pub fn install_path(&self) -> PathBuf {
        self.base.install_path()
    }

    /// `cores/lpc8xx/` — the core sources + headers (Arduino.h,
    /// HardwareSerial, startup, main, wiring, etc.).
    pub fn core_dir(&self) -> PathBuf {
        self.base.install_path().join("cores").join("lpc8xx")
    }

    /// `variants/<variant>/` — board pin map + variant glue.
    pub fn variant_dir(&self, variant: &str) -> PathBuf {
        self.base.install_path().join("variants").join(variant)
    }

    /// Resolve a board-relative `ldscript` path (e.g.
    /// `linker_scripts/gcc/lpc845_flash.ld`) against the install root.
    pub fn linker_script(&self, ldscript_rel: &str) -> PathBuf {
        self.base.install_path().join(ldscript_rel)
    }
}

impl crate::Package for ArduinoCoreLpc8xx {
    fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.base.install_path());
        }

        let rt = tokio::runtime::Handle::try_current().ok();
        if let Some(handle) = rt {
            handle.block_on(self.base.staged_install(Self::validate))?;
        } else {
            let rt = tokio::runtime::Runtime::new().map_err(|e| {
                fbuild_core::FbuildError::PackageError(format!(
                    "failed to create tokio runtime: {}",
                    e
                ))
            })?;
            rt.block_on(self.base.staged_install(Self::validate))?;
        }

        Ok(self.base.install_path())
    }

    fn is_installed(&self) -> bool {
        self.base.is_cached()
            && self
                .base
                .install_path()
                .join("cores")
                .join("lpc8xx")
                .join("main.cpp")
                .exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn not_installed_on_empty_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pkg = ArduinoCoreLpc8xx::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!pkg.is_installed());
    }

    #[test]
    fn validate_rejects_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(ArduinoCoreLpc8xx::validate(tmp.path()).is_err());
    }

    #[test]
    fn accessors_compose_expected_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pkg = ArduinoCoreLpc8xx::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(pkg.core_dir().ends_with("cores/lpc8xx"));
        assert!(pkg.variant_dir("lpc845brk").ends_with("variants/lpc845brk"));
        assert!(pkg
            .linker_script("linker_scripts/gcc/lpc845_flash.ld")
            .ends_with("linker_scripts/gcc/lpc845_flash.ld"));
    }

    #[test]
    fn commit_is_pinned() {
        assert_eq!(ArduinoCoreLpc8xx::commit().len(), 40);
    }
}

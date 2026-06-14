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
///
/// Pinned to the Wire/SPI proxy-singleton refactor merge
/// (zackees/ArduinoCore-LPC8xx#27): `TwoWire Wire;` and `SPIClass SPI;` are
/// now trivial proxy facades that lazily construct their impl via a Meyers
/// singleton — `--gc-sections` can drop the entire I2C / SPI driver from
/// sketches that don't reference `Wire` / `SPI`.
///
/// Earlier merges still in effect:
///   #24: `operator new`/`new[]`, `.ARM.exidx`, forced heap base, F_CPU=24MHz
///   #25: drop unsigned-long operator new overloads (32-bit ABI)
///   #26: collapse operator delete variants to a single free thunk
const ACLPC_COMMIT: &str = "195a2eddd31eba8472ceaffa6a1a1902f72439ae";
const ACLPC_VERSION: &str = "0.1.0+g195a2ed";
const ACLPC_URL: &str =
    "https://github.com/zackees/ArduinoCore-LPC8xx/archive/195a2eddd31eba8472ceaffa6a1a1902f72439ae.tar.gz";
const ACLPC_CHECKSUM: &str = "366de74b93179ba4b5e8ac9e527f15f189d9e26bcedd030fe11c11a2c907f3e4";

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
        let root = find_core_root(install_dir);
        for rel in ["cores/lpc8xx/main.cpp", "cores/lpc8xx/startup_lpc8xx.c"] {
            if !root.join(rel).exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "ArduinoCore-LPC8xx missing {} (in {})",
                    rel,
                    root.display()
                )));
            }
        }
        Ok(())
    }

    /// Repository root of the vendored package. Used as the linker `-L`
    /// search root so a linker script's relative `INCLUDE` directive
    /// (e.g. `INCLUDE linker_scripts/gcc/lpc8xx_common.ld`) resolves.
    ///
    /// GitHub's archive tarball wraps the repo contents in a top-level
    /// `ArduinoCore-LPC8xx-<sha>/` directory; `find_core_root` strips that.
    pub fn install_path(&self) -> PathBuf {
        find_core_root(&self.base.install_path())
    }

    /// `cores/lpc8xx/` — the core sources + headers (Arduino.h,
    /// HardwareSerial, startup, main, wiring, etc.).
    pub fn core_dir(&self) -> PathBuf {
        self.install_path().join("cores").join("lpc8xx")
    }

    /// `variants/<variant>/` — board pin map + variant glue.
    pub fn variant_dir(&self, variant: &str) -> PathBuf {
        self.install_path().join("variants").join(variant)
    }

    /// Resolve a board-relative `ldscript` path (e.g.
    /// `linker_scripts/gcc/lpc845_flash.ld`) against the install root.
    pub fn linker_script(&self, ldscript_rel: &str) -> PathBuf {
        self.install_path().join(ldscript_rel)
    }
}

/// Find the actual repository root inside an extracted GitHub archive.
///
/// GitHub's archive tarball extracts as `ArduinoCore-LPC8xx-<sha>/<contents>`,
/// wrapping all repo files inside an extra directory. Descend one level if the
/// expected `cores/` layout isn't directly at `install_dir`.
fn find_core_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("cores").exists() {
        return install_dir.to_path_buf();
    }
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("cores").exists() {
                return path;
            }
        }
    }
    install_dir.to_path_buf()
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
            && find_core_root(&self.base.install_path())
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

//! Arduino core framework for NXP LPC8xx (`FastLED/framework-arduino-lpc8xx`).
//!
//! Downloads and vendors the real Arduino-compatible core
//! ([FastLED/framework-arduino-lpc8xx](https://github.com/FastLED/framework-arduino-lpc8xx))
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
/// Pinned to the full-NXP-CMSIS-PAL merge
/// (FastLED/framework-arduino-lpc8xx#34): `variants/lpc845/LPC845.h` and
/// `variants/lpc804/LPC804.h` now carry the full NXP CMSIS Peripheral
/// Access Layer (~10K / 7K lines, BSD-3-Clause, rev. 1.2) instead of the
/// previous 54/49-line IRQn-only stubs. This unblocks downstream FastLED
/// #3437 — the FastLED LPC drivers now consume canonical NXP typedefs
/// (`SCT_Type`, `DMA_Type`, `SYSCON_Type`, `SPI_Type`, `PLU_Type`,
/// pointer macros `SCT0`/`DMA0`/`SYSCON`/`SPI0`/`SPI1`/`PLU`) directly
/// instead of hand-rolling parallel register-map shims.
///
/// Earlier merges still in effect:
///   #27: Wire/SPI proxy-singleton refactor (--gc-sections drops unused I2C/SPI)
///   #24-#26: operator new/delete + .ARM.exidx + heap base + F_CPU=24MHz
///
/// Bump post the phantom-LPC804-DMA revert
/// (FastLED/framework-arduino-lpc8xx#36). The previous pin (`1179200`)
/// added a fabricated `DMA_Type` + `DMA0` block to
/// `variants/lpc804/LPC804.h` at reserved AHB slot 0x50008000; LPC804
/// silicon has no DMA peripheral (NXP mcux-sdk has zero `DMA_Type`,
/// `DMA0_BASE`, `FSL_FEATURE_SOC_DMA_COUNT` for LPC804; UM11065 has
/// no DMA chapter). Diagnosed by @phatpaul in FastLED/FastLED#3499
/// comment 4855252061. FastLED-side revert cascade:
/// FastLED/FastLED#3513 (merged). FastLED#3499 closed as INVALID.
/// Preventive guardrail: FastLED/FastLED#3506 / #3507
/// (`agents/docs/peripheral-existence.md`).
///
/// 0.2.3+g9e8be02 (framework-arduino-lpc8xx#38): named weak IRQ
/// handlers in the LPC845/LPC804 startup vector tables + weak NMI
/// alias. Unblocks FastLED ISR-driven drivers (DMA chunk-chain refill
/// for SPI/UART async streaming — FastLED/FastLED#3453 follow-up).
const ACLPC_COMMIT: &str = "9e8be028892e6eeb4796b46d0069dbb1a39a47a9";
const ACLPC_VERSION: &str = "0.2.3+g9e8be02";
const ACLPC_URL: &str =
    "https://github.com/FastLED/framework-arduino-lpc8xx/archive/9e8be028892e6eeb4796b46d0069dbb1a39a47a9.tar.gz";
// SHA256 of the archive GitHub currently serves for
// `github.com/FastLED/framework-arduino-lpc8xx/archive/9e8be02889…tar.gz`.
// Verified 2026-07-02 via `curl … | sha256sum`.
const ACLPC_CHECKSUM: &str = "c295ed4204eb123f735538196dd431bc8ebbe3bac86b0ffde32d6bfbab484758";

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

    /// Construct with a consumer-supplied override (parsed from the env's
    /// `platform_packages` line in `platformio.ini`). The default const-pinned
    /// URL / version / checksum are replaced; `cache_subdir` and `name` are
    /// preserved. See `PackageBase::with_override` and FastLED/fbuild#681
    /// (the consolidation of the LPC8xx-specific path introduced in #663).
    pub fn with_override(project_dir: &Path, ovr: fbuild_config::PackageOverride) -> Self {
        Self {
            base: PackageBase::new(
                "framework-arduino-lpc8xx",
                ACLPC_VERSION,
                ACLPC_URL,
                ACLPC_URL,
                Some(ACLPC_CHECKSUM),
                CacheSubdir::Platforms,
                project_dir,
            )
            .with_override(ovr),
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

#[async_trait::async_trait]
impl crate::Package for ArduinoCoreLpc8xx {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.base.install_path());
        }

        self.base.staged_install(Self::validate).await?;

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

    // Override cache-key uniqueness is asserted at the abstraction level in
    // `crates/fbuild-packages/src/lib.rs::package_override_tests` so the
    // invariant doesn't need to be re-proved per-package.
}

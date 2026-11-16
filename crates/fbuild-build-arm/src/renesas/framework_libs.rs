//! Bundled (framework-shipped) Arduino libraries for the Renesas UNO cores.
//!
//! The Arduino renesas_uno core ships a `libraries/` directory holding
//! `WiFiS3`, `SPI`, `Wire`, `RTC`, `Arduino_CAN`, ... . None of it was ever
//! put on the include path or compiled, so a sketch that did
//! `#include <WiFiS3.h>` failed at the include itself with
//! `fatal error: WiFiS3.h: No such file or directory`
//! (FastLED/fbuild#2020). Arduino CLI builds the same sketch against
//! `arduino:renesas_uno` 1.6.0.
//!
//! [`select_bundled_libraries`] runs the same library-dependency finder the
//! Teensy, STM32, RP2040 and ESP8266 orchestrators use — seeded from the
//! project's translation units, honoring `lib_deps` and active `#if` branches,
//! with the compiler's include path so SDK headers resolve as they will at
//! compile time. Only the libraries the sketch actually reaches contribute
//! include dirs and sources, so an unreached library (`Arduino_FreeRTOS`,
//! `LittleFilesystem`, ...) can never shadow a core header.

use std::collections::HashMap;
use std::path::Path;

use fbuild_build_engine::framework_libs::{
    resolve_framework_library_selection_active_declared_with_extra,
    warn_if_lib_ldf_mode_unsupported,
};
use fbuild_core::Result;
use fbuild_core::path::NormalizedPath;
use fbuild_packages::library::FrameworkLibrary;

/// Bundled libraries one project actually reaches.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct BundledLibraries {
    /// Reached library names, sorted and deduplicated (logging and tests).
    pub names: Vec<String>,
    /// Sources of those libraries, to be added to the core compile set.
    pub sources: Vec<NormalizedPath>,
    /// Include dirs of those libraries, to be added to the compiler's path.
    pub include_dirs: Vec<NormalizedPath>,
}

impl BundledLibraries {
    fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.include_dirs.is_empty()
    }
}

/// Resolve which bundled Arduino libraries `project_dir` uses.
///
/// `libraries` comes from `RenesasCores::get_framework_libraries`;
/// `compiler_include_dirs` must be the include path built so far (core,
/// variant, FSP, project, toolchain) so the finder resolves headers exactly as
/// the compiler will. Every reached library's sources and include dirs are
/// returned.
pub(super) fn select_bundled_libraries(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
    defines: &HashMap<String, String>,
    declared: &[String],
    compiler_include_dirs: &[NormalizedPath],
) -> Result<BundledLibraries> {
    if libraries.is_empty() {
        return Ok(BundledLibraries::default());
    }
    let search_path = compiler_include_dirs
        .iter()
        .map(|dir| dir.as_path().to_path_buf())
        .collect::<Vec<_>>();
    let selection = resolve_framework_library_selection_active_declared_with_extra(
        libraries,
        project_dir,
        src_dir,
        defines,
        declared,
        &[],
        &search_path,
    )?;
    Ok(BundledLibraries {
        names: selection.required_libraries,
        sources: selection
            .source_files
            .into_iter()
            .map(NormalizedPath::from)
            .collect(),
        include_dirs: selection
            .include_dirs
            .into_iter()
            .map(NormalizedPath::from)
            .collect(),
    })
}

/// Warn when a project sets a `lib_ldf_mode` fbuild does not implement.
pub(super) fn warn_unsupported_ldf_mode(mode: Option<&str>) {
    warn_if_lib_ldf_mode_unsupported(mode);
}

/// Log the resolved selection.
pub(super) fn log_selection(bundled: &BundledLibraries) {
    if bundled.is_empty() {
        tracing::debug!("bundled Arduino libraries: none reached by this project");
        return;
    }
    tracing::info!(
        "bundled Arduino libraries: compiled {} source file(s) from {} ({})",
        bundled.sources.len(),
        bundled.names.len(),
        bundled.names.join(", "),
    );
}

#[cfg(test)]
#[path = "framework_libs_tests.rs"]
mod tests;

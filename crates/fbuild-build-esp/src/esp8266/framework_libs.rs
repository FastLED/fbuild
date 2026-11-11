//! Bundled (framework-shipped) Arduino libraries for ESP8266.
//!
//! The Arduino ESP8266 core ships a `libraries/` directory holding
//! `ESP8266WiFi`, `ESP8266WebServer`, `ESP8266mDNS`, `WiFiUdp`, ... . Their
//! headers were always on the include path, but nothing ever compiled their
//! sources: a sketch that included one of them compiled and then failed at
//! link time with undefined references to the library's symbols
//! (FastLED/fbuild#1994).
//!
//! [`select_bundled_libraries`] runs the same library-dependency finder the
//! Teensy, STM32 and RP2040 orchestrators use — seeded from the project's
//! translation units, honoring `lib_deps` and active `#if` branches — so only
//! the libraries the sketch actually reaches join the compile set.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_build_engine::framework_libs::{
    resolve_framework_library_selection_active_declared_with_extra,
    warn_if_lib_ldf_mode_unsupported,
};
use fbuild_core::Result;
use fbuild_packages::library::FrameworkLibrary;

/// Bundled libraries one project actually reaches.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct BundledLibraries {
    /// Reached library names, sorted and deduplicated (logging and tests).
    pub names: Vec<String>,
    /// Sources of those libraries, to be added to the core compile set.
    pub sources: Vec<PathBuf>,
}

impl BundledLibraries {
    fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

/// Resolve which bundled Arduino libraries `project_dir` uses.
///
/// `libraries` comes from [`Esp8266Framework::get_framework_libraries`];
/// `compiler_include_dirs` must be the compiler's complete include path so the
/// finder resolves headers exactly as the compiler will (capability macros
/// live in SDK headers). Every reached library's sources are returned for
/// compilation.
pub(super) fn select_bundled_libraries(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
    defines: &HashMap<String, String>,
    declared: &[String],
    compiler_include_dirs: &[PathBuf],
) -> Result<BundledLibraries> {
    if libraries.is_empty() {
        return Ok(BundledLibraries::default());
    }
    let selection = resolve_framework_library_selection_active_declared_with_extra(
        libraries,
        project_dir,
        src_dir,
        defines,
        declared,
        &[],
        compiler_include_dirs,
    )?;
    Ok(BundledLibraries {
        names: selection.required_libraries,
        sources: selection.source_files,
    })
}

/// Warn when a project sets a `lib_ldf_mode` fbuild does not implement.
pub(super) fn warn_unsupported_ldf_mode(mode: Option<&str>) {
    warn_if_lib_ldf_mode_unsupported(mode);
}

/// Log the resolved selection at the same level the ESP32 path does.
pub(super) fn log_selection(bundled: &BundledLibraries) {
    if bundled.is_empty() {
        tracing::debug!("framework-libs cache: no bundled Arduino library reached");
        return;
    }
    tracing::info!(
        libraries = %bundled.names.join(", "),
        sources = bundled.sources.len(),
        "bundled Arduino libraries: compiled {} source file(s) from {}",
        bundled.sources.len(),
        bundled.names.len(),
    );
}

#[cfg(test)]
#[path = "framework_libs_tests.rs"]
mod tests;

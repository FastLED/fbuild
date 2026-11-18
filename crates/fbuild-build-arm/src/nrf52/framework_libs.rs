//! Bundled (framework-shipped) Arduino libraries for the nRF52 cores.
//!
//! The Adafruit nRF52 BSP (and Seeed's fork) ship a `libraries/` tree next to
//! the core: `Wire`, `SPI`, `Adafruit_LittleFS`, `Bluefruit52Lib`,
//! `Servo`, ... . The orchestrator used to put only `Wire/` and `SPI/` on the
//! include path and never compiled either library, so a sketch calling
//! `Wire.begin()` compiled and then failed to link with
//! `undefined reference to TwoWire::begin()` — on `main` as well
//! (FastLED/fbuild#2072).
//!
//! [`select_bundled_libraries`] runs the same library-dependency finder the
//! Teensy, STM32, RP2040, ESP8266 and Renesas orchestrators use — seeded from
//! the project's translation units, honoring `lib_deps` and active `#if`
//! branches, with the compiler's include path so SDK headers resolve as they
//! will at compile time. Only the libraries the sketch actually reaches
//! contribute include dirs and sources, so an unreached library
//! (`Bluefruit52Lib` is large) can never shadow a core header or bloat the
//! build.
//!
//! `Adafruit_TinyUSB_Arduino` is deliberately not a candidate: the TinyUSB
//! stack is already compiled unconditionally by the orchestrator's dedicated
//! block (USB CDC on every nRF52840 build), including the `SPI.cpp` the host
//! adapter needs, so handing it to the finder again would only duplicate
//! translation units.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use fbuild_build_engine::framework_libs::{
    resolve_framework_library_selection_active_declared_with_extra,
    warn_if_lib_ldf_mode_unsupported,
};
use fbuild_core::Result;
use fbuild_core::path::NormalizedPath;
use fbuild_packages::library::framework_library::discover_framework_libraries;

/// Libraries whose sources the orchestrator compiles itself.
const ORCHESTRATOR_COMPILED_LIBRARIES: &[&str] = &["Adafruit_TinyUSB_Arduino"];

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
/// `libraries_dir` is the core's `libraries/` (the same directory
/// `fbuild lib-select` inspects, FastLED/fbuild#2042). `already_compiled`
/// holds sources the orchestrator has already added to the core set (the
/// TinyUSB block and the `SPI.cpp` the nRF52 TinyUSB host adapter needs);
/// duplicates are filtered out so no translation unit is built twice.
/// `compiler_include_dirs` must be the include path built so far (core,
/// variant, SDK, project, toolchain) so the finder resolves headers exactly
/// as the compiler will.
pub(super) fn select_bundled_libraries(
    libraries_dir: &Path,
    project_dir: &Path,
    src_dir: &Path,
    defines: &HashMap<String, String>,
    declared: &[String],
    compiler_include_dirs: &[NormalizedPath],
    already_compiled: &[NormalizedPath],
) -> Result<BundledLibraries> {
    let libraries = discover_framework_libraries(libraries_dir);
    if libraries.is_empty() {
        return Ok(BundledLibraries::default());
    }
    let candidates: Vec<_> = libraries
        .into_iter()
        .filter(|library| !ORCHESTRATOR_COMPILED_LIBRARIES.contains(&library.name.as_str()))
        .collect();
    let search_path = compiler_include_dirs
        .iter()
        .map(|dir| dir.as_path().to_path_buf())
        .collect::<Vec<_>>();
    let selection = resolve_framework_library_selection_active_declared_with_extra(
        &candidates,
        project_dir,
        src_dir,
        defines,
        declared,
        &[],
        &search_path,
    )?;
    let compiled: HashSet<&NormalizedPath> = already_compiled.iter().collect();
    let sources: Vec<NormalizedPath> = selection
        .source_files
        .into_iter()
        .map(NormalizedPath::from)
        .filter(|source| !compiled.contains(source))
        .collect();
    Ok(BundledLibraries {
        names: selection.required_libraries,
        sources,
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
        tracing::debug!("nrf52 framework libraries: none reached by this project");
        return;
    }
    tracing::info!(
        "nrf52 framework libraries: compiled {} source file(s) from {} ({})",
        bundled.sources.len(),
        bundled.names.len(),
        bundled.names.join(", "),
    );
}

#[cfg(test)]
#[path = "framework_libs_tests.rs"]
mod tests;

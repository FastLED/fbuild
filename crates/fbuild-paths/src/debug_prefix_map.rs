//! Debug-info path stability: make `DW_AT_comp_dir` independent of the project
//! location.
//!
//! The compiler records its working directory in the debug info, so two
//! byte-identical sketches in two directories produce different objects and no
//! compiler cache can reuse them (FastLED/fbuild#1347, #1414 finding 1).
//! `-fdebug-prefix-map=<project>=.` records that directory — and every source
//! path under it — identically in every checkout.
//!
//! Split out of `lib.rs` to keep that file under the 1000-LOC gate.

use std::path::Path;

use fbuild_core::path::normalize_for_key;

use crate::BuildLayout;

/// The replacement root used by [`debug_prefix_map_args`]: `DW_AT_comp_dir`
/// becomes `.`, i.e. relative to the project root.
pub const DEBUG_PREFIX_MAP_ROOT: &str = ".";

impl BuildLayout {
    /// `-fdebug-prefix-map` arguments that make the debug info this build
    /// writes independent of where the project lives.
    ///
    /// The compiler records its working directory as `DW_AT_comp_dir`, so two
    /// byte-identical sketches in two directories produce different objects and
    /// no compiler cache can reuse them (FastLED/fbuild#1347, #1414 finding 1).
    /// Mapping the project root to `.` records that directory, and every source
    /// path under it, identically in every checkout.
    ///
    /// `-fdebug-prefix-map` rather than `-ffile-prefix-map` is deliberate:
    /// the umbrella spelling is GCC 8+, while avr-gcc 7.3 and the legacy ESP32
    /// GCC 5 only have the debug variant, and `__FILE__` is already covered by
    /// the `-fmacro-prefix-map` flags. Debug info is the part that is not.
    ///
    /// Debuggers resolve the recorded `.` against their own working
    /// directory, so start gdb/lldb from the project root — the source names it
    /// then looks for (`src/main.cpp`) are exactly the ones on disk.
    pub fn debug_prefix_map_args(&self) -> Vec<String> {
        debug_prefix_map_args(&self.project_dir, &self.resolve())
    }
}

/// See [`BuildLayout::debug_prefix_map_args`].
///
/// `DW_AT_comp_dir` is the compile workspace, which
/// `fbuild_core::path::compile_cwd_from_output` anchors at the project root
/// (source arguments are already project-relative there), so mapping the
/// project root to `.` makes the recorded directory and every recorded source
/// path location-independent.
///
/// A build root outside the project (`FBUILD_BUILD_DIR` or an explicit
/// override) is mapped as well, to a distinct stable label: stages that
/// compile into it without a `.fbuild` component pass absolute paths, and
/// those would otherwise keep the machine-specific tree.
pub fn debug_prefix_map_args(project_dir: &Path, build_dir: &Path) -> Vec<String> {
    let mut args = vec![format!(
        "-fdebug-prefix-map={}={DEBUG_PREFIX_MAP_ROOT}",
        project_dir.display()
    )];
    if !is_within(build_dir, project_dir) {
        args.push(format!(
            "-fdebug-prefix-map={}=fbuild-build",
            build_dir.display()
        ));
    }
    args
}

/// Whether `candidate` is `root` itself or lives beneath it.
///
/// Compares `fbuild_core::path::normalize_for_key` spellings rather than the
/// raw `Path::starts_with`, so a canonicalized-vs-raw pair (or a separator /
/// case difference) cannot send a build root that *is* under the project down
/// the "external build root" branch and emit a second, conflicting map
/// (FastLED/fbuild#952).
fn is_within(candidate: &Path, root: &Path) -> bool {
    let candidate_key = normalize_for_key(candidate);
    let root_key = normalize_for_key(root);
    if candidate_key == root_key {
        return true;
    }
    let mut prefix = root_key;
    if !prefix.ends_with('/') {
        prefix.push('/');
    }
    candidate_key.starts_with(&prefix)
}

#[cfg(test)]
#[path = "debug_prefix_map_tests.rs"]
mod tests;

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

use fbuild_core::path::{NormalizedPath, normalize_for_key};

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
    prefix_map_args(project_dir, build_dir, &["-fdebug-prefix-map"])
}

/// Prefix-map arguments that cover **every** path-bearing channel a GCC 8+
/// or clang toolchain understands: `-ffile-prefix-map`,
/// `-fmacro-prefix-map` and `-fdebug-prefix-map`.
///
/// [`debug_prefix_map_args`] alone makes the *object bytes* independent of
/// the project directory, but it is not enough for a shared cache key: a
/// compiler-cache may only reuse an object across checkouts once the argv
/// proves that file, macro and debug path remapping all cover the checkout
/// root (zccache's `cc_prefix_maps_cover_root`). With only the debug channel
/// mapped, the entry stays scoped to one worktree, so a brand-new project
/// directory recompiles the sketch even though the artifact it would reuse is
/// byte-identical (FastLED/fbuild#1414).
///
/// Use this on toolchains that accept all three flags (GCC 8+, clang). Older
/// GCC releases (avr-gcc 7.3, the legacy ESP32 GCC 5) only have
/// `-fdebug-prefix-map`; call [`debug_prefix_map_args`] there.
pub fn portable_prefix_map_args(project_dir: &Path, build_dir: &Path) -> Vec<String> {
    prefix_map_args(
        project_dir,
        build_dir,
        &[
            "-ffile-prefix-map",
            "-fmacro-prefix-map",
            "-fdebug-prefix-map",
        ],
    )
}

/// Shared body of the two public spellings: one flag per (channel, root).
fn prefix_map_args(project_dir: &Path, build_dir: &Path, channels: &[&str]) -> Vec<String> {
    // Forward slashes, not the native rendering: the whole point of these
    // flags is that the recorded path is identical in every checkout, and that
    // includes checkouts on another OS. Native separators made the same
    // project emit `/p/sketch=...` on Linux and a backslash-spelled variant on Windows,
    // so the core cache could never share objects across hosts
    // (FastLED/fbuild#1780). Both spellings compile; only one is stable.
    let mut roots = vec![(
        NormalizedPath::new(project_dir).display_slash(),
        DEBUG_PREFIX_MAP_ROOT.to_string(),
    )];
    if !is_within(build_dir, project_dir) {
        roots.push((
            NormalizedPath::new(build_dir).display_slash(),
            "fbuild-build".to_string(),
        ));
    }
    let mut args = Vec::with_capacity(roots.len() * channels.len());
    for (root, label) in roots {
        for channel in channels {
            args.push(format!("{channel}={root}={label}"));
        }
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
